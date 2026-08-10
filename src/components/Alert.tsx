interface AlertProps {
  title?: string;
  message: string;
}

/** An error message. Uses `role="alert"` so screen readers announce it when
 *  it appears mid-task. */
export function Alert({ title, message }: AlertProps) {
  return (
    <div className="alert" role="alert">
      <div className="alert__body">
        {title && <div className="alert__title">{title}</div>}
        <div>{message}</div>
      </div>
    </div>
  );
}
