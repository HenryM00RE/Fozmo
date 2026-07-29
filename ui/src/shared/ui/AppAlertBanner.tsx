import { Icon } from './Icon';

/**
 * A refusal the listener has to read before it means anything.
 *
 * The transient notice in the toolbar is right for "that didn't work, try
 * again"; it is wrong for something the listener must act on somewhere else
 * before their request can succeed. This stays put until it is dismissed or
 * replaced.
 */
export function AppAlertBanner({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return (
    <div className="app-alert-banner" role="alert" data-testid="app-alert-banner">
      <span className="app-alert-banner-message">{message}</span>
      <button
        className="app-alert-banner-dismiss"
        type="button"
        title="Dismiss"
        aria-label="Dismiss"
        onClick={onDismiss}
      >
        <Icon path="M18 6 6 18M6 6l12 12" />
      </button>
    </div>
  );
}
