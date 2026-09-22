#!/usr/bin/env python3
"""Runs a GGUF model over a dataset split and records its predictions.

# What this is for

Establishing what the **current** production model scores on the Phase 2
held-out test set, before any fine-tuning, so that a later fine-tuned model
has something honest to be compared against. It is also the script that
evaluates the fine-tuned model afterwards, against the same split with the
same prompt — a comparison is only meaningful if both sides were asked the
same question.

# It does not touch production

The model file and runtime are **read**, never written, and are the ones an
operator already provisioned (`ai/models/llm/`, `ai/runtime/llama-cpu/`; see
docs/ai/PROVISIONING.md). `llama-server` is started as a child process on a
port chosen to avoid the ones SecureMesh uses, and is killed on exit.
Nothing under `src-tauri/` is read, written, imported or executed, and
`LlamaConfig` is not involved — this script reaches the runtime directly the
same way the Rust adapter does.

# Network

Loopback only, via `urllib` against `127.0.0.1`. The server is started with
`--host 127.0.0.1` so it binds no external interface, and there is no
hostname parameter anywhere on this path — the same property
`src-tauri/src/ai/loopback_http.rs` is built around. No model is downloaded:
a missing file is an error, never a fetch.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

import securemesh_prompt as smp  # noqa: E402
import validate_dataset as vd  # noqa: E402

TRAINING_ROOT = SCRIPT_DIR.parent
REPO_ROOT = TRAINING_ROOT.parent

DEFAULT_MODEL = REPO_ROOT / "ai" / "models" / "llm" / "qwen2.5-1.5b-instruct-q4_k_m.gguf"
DEFAULT_SERVER = REPO_ROOT / "ai" / "runtime" / "llama-cpu" / "llama-server.exe"

# Away from the ports a running SecureMesh node uses for its own generation
# and embedding servers, so an evaluation cannot collide with a live node.
DEFAULT_PORT = 18422

# Mirrors LlamaConfig::generation() (src-tauri/src/ai/llama.rs:67-79).
CONTEXT_TOKENS = 2048
THREADS = 8

STARTUP_TIMEOUT_S = 180
REQUEST_TIMEOUT_S = 180


def loopback_post(port: int, path: str, body: str, timeout: float) -> str:
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}{path}",
        data=body.encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read().decode("utf-8")


def loopback_get(port: int, path: str, timeout: float) -> str:
    request = urllib.request.Request(f"http://127.0.0.1:{port}{path}", method="GET")
    with urllib.request.urlopen(request, timeout=timeout) as response:
        return response.read().decode("utf-8")


def start_server(server_binary: Path, model_path: Path, port: int) -> subprocess.Popen:
    """Starts llama-server on loopback and waits until it answers /health."""
    command = [
        str(server_binary),
        "-m", str(model_path),
        "--host", "127.0.0.1",
        "--port", str(port),
        "-c", str(CONTEXT_TOKENS),
        "-t", str(THREADS),
        "--no-warmup",
    ]
    print(f"starting runtime: {' '.join(command)}")
    creation_flags = 0x08000000 if sys.platform == "win32" else 0  # CREATE_NO_WINDOW
    process = subprocess.Popen(
        command,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
        creationflags=creation_flags,
    )

    deadline = time.time() + STARTUP_TIMEOUT_S
    while time.time() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"the runtime exited during startup (code {process.returncode})")
        try:
            loopback_get(port, "/health", 1.0)
            print(f"runtime ready on 127.0.0.1:{port}")
            return process
        except (urllib.error.URLError, OSError):
            time.sleep(0.25)

    process.kill()
    raise RuntimeError("the runtime did not become ready in time")


def analyse(port: int, report_text: str, schema: dict) -> tuple[str, int]:
    """One schema-constrained analysis. Returns (raw output, latency in ms).

    Mirrors LlamaServerEngine::generate_structured (ai/llama.rs:367-383):
    same endpoint, same temperature, same `json_schema` constraint.
    """
    body = json.dumps(
        {
            "messages": smp.chat_messages(report_text),
            "temperature": 0.0,
            "max_tokens": smp.ANALYSIS_TOKENS,
            "stream": False,
            "json_schema": schema,
        }
    )
    started = time.perf_counter()
    response = loopback_post(port, "/v1/chat/completions", body, REQUEST_TIMEOUT_S)
    latency_ms = int((time.perf_counter() - started) * 1000)

    parsed = json.loads(response)
    content = parsed.get("choices", [{}])[0].get("message", {}).get("content")
    if content is None:
        raise RuntimeError("the runtime returned a reply with no content")
    return content, latency_ms


def load_split(path: Path) -> list[dict]:
    records = []
    with path.open("r", encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                records.append(json.loads(line))
    return records


def main() -> int:
    vd._fix_windows_console_encoding()
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--split",
        type=Path,
        default=TRAINING_ROOT / "data" / "processed" / "test.jsonl",
        help="Dataset split to run against (default: the held-out test set)",
    )
    parser.add_argument("--model", type=Path, default=DEFAULT_MODEL)
    parser.add_argument("--server", type=Path, default=DEFAULT_SERVER)
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    parser.add_argument(
        "--out",
        type=Path,
        required=True,
        help="Where to write the predictions JSONL",
    )
    parser.add_argument(
        "--limit", type=int, help="Only run the first N records (for a smoke test)"
    )
    parser.add_argument(
        "--label",
        default="base-qwen2.5-1.5b-instruct-q4_k_m",
        help="Name recorded with the predictions, so results can be told apart",
    )
    args = parser.parse_args()

    problems = []
    if not args.model.exists():
        problems.append(f"model not found: {args.model}")
    if not args.server.exists():
        problems.append(f"llama-server not found: {args.server}")
    if not args.split.exists():
        problems.append(f"split not found: {args.split}")
    if problems:
        for problem in problems:
            print(f"  - {problem}")
        print(
            "\nThis script never downloads a model or a runtime. Provision them "
            "locally first — see docs/ai/PROVISIONING.md."
        )
        return 1

    records = load_split(args.split)
    if args.limit:
        records = records[: args.limit]

    print(f"model:  {args.model.name}")
    print(f"split:  {args.split.name} ({len(records)} records)")

    schema = smp.analysis_schema()
    process = start_server(args.server, args.model, args.port)

    predictions = []
    failures = 0
    started_all = time.perf_counter()
    try:
        for index, record in enumerate(records, start=1):
            try:
                raw, latency_ms = analyse(args.port, record["report_text"], schema)
                predictions.append(
                    {
                        "id": record["id"],
                        "predicted": raw,
                        "latency_ms": latency_ms,
                        "model": args.label,
                    }
                )
            except Exception as error:  # noqa: BLE001 - one bad record must not end the run
                failures += 1
                predictions.append(
                    {
                        "id": record["id"],
                        "predicted": None,
                        "error": f"{type(error).__name__}: {error}",
                        "model": args.label,
                    }
                )
            if index % 20 == 0 or index == len(records):
                elapsed = time.perf_counter() - started_all
                print(
                    f"  {index}/{len(records)} done "
                    f"({elapsed:.0f}s elapsed, {elapsed / index:.1f}s/record)"
                )
    finally:
        process.kill()
        process.wait()
        print("runtime stopped")

    total_s = time.perf_counter() - started_all
    args.out.parent.mkdir(parents=True, exist_ok=True)
    with args.out.open("w", encoding="utf-8") as handle:
        for prediction in predictions:
            handle.write(json.dumps(prediction, ensure_ascii=False))
            handle.write("\n")

    print(f"\n{len(predictions)} predictions written to {args.out}")
    print(f"{failures} request failure(s); total wall time {total_s:.0f}s")
    return 0


if __name__ == "__main__":
    sys.exit(main())
