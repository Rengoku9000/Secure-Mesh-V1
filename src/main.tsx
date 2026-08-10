import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// Tokens first: every other stylesheet depends on the custom properties they
// define.
import "./styles/tokens.css";
import "./styles/app.css";

const container = document.getElementById("root");
if (!container) {
  throw new Error("SecureMesh could not find its root element.");
}

ReactDOM.createRoot(container).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
