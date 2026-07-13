export function SetupNotice({
  actionLabel,
  message,
  onAction
}: {
  actionLabel: string;
  message: string;
  onAction: () => void;
}) {
  return (
    <div className="setup-notice" role="status">
      <span>{message}</span>
      <button type="button" onClick={onAction}>
        {actionLabel}
      </button>
    </div>
  );
}
