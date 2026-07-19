import { useEffect, useId, useState } from 'react';
import { qobuzAccountStatusLabel } from '../features/settings/model/qobuzSettingsModel';
import { isHostDeviceBrowser } from '../features/settings/settingsModel';
import { storageKey } from '../shared/identity';
import { endpoints } from '../shared/lib/api';
import type { JsonRecord, RouteState } from '../shared/types';
import { Icon } from '../shared/ui/Icon';
import { Modal } from '../shared/ui/Modal';

const GETTING_STARTED_COMPLETE_KEY = storageKey('GettingStartedV2Complete');
const STEP_COUNT = 4;

type GuideStorage = Pick<Storage, 'getItem' | 'setItem'>;
type GuideStep = 1 | 2 | 3 | 4;

export function shouldShowGettingStartedGuide(storage: GuideStorage | null = safeLocalStorage()) {
  if (!storage) return true;
  try {
    return storage.getItem(GETTING_STARTED_COMPLETE_KEY) !== '1';
  } catch {
    return true;
  }
}

export function shouldShowGettingStartedGuideOnHost(
  storage: GuideStorage | null = safeLocalStorage(),
  hostname?: string
) {
  return isHostDeviceBrowser(hostname) && shouldShowGettingStartedGuide(storage);
}

export function qobuzEnabledForGettingStarted(qobuzStatus: JsonRecord | null) {
  return Boolean(qobuzStatus?.initialized || qobuzStatus?.logged_in || qobuzStatus?.authenticated);
}

export function markGettingStartedGuideComplete(storage: GuideStorage | null = safeLocalStorage()) {
  if (!storage) return;
  try {
    storage.setItem(GETTING_STARTED_COMPLETE_KEY, '1');
  } catch {
    // Storage can be unavailable in private or locked-down browsers. The
    // guide still closes for this page load and may appear again next time.
  }
}

function safeLocalStorage(): Storage | null {
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function stepTitle(step: GuideStep) {
  if (step === 1) return 'Welcome to Fozmo';
  if (step === 2) return 'Metadata for local albums';
  if (step === 3) return 'Add your music folder';
  return 'Connect Qobuz';
}

export function FirstRunGuide({
  onNavigate,
  qobuzStatus,
  qobuzStatusLoaded
}: {
  onNavigate: (route: RouteState) => void;
  qobuzStatus: JsonRecord | null;
  qobuzStatusLoaded: boolean;
}) {
  const titleId = useId();
  const [open, setOpen] = useState(() => shouldShowGettingStartedGuideOnHost());
  const [step, setStep] = useState<GuideStep>(1);
  const [folderBusy, setFolderBusy] = useState(false);
  const [folderMessage, setFolderMessage] = useState('');
  const [folderAdded, setFolderAdded] = useState(false);
  const qobuzEnabled = qobuzEnabledForGettingStarted(qobuzStatus);
  const qobuzConnected = Boolean(qobuzStatus?.logged_in || qobuzStatus?.authenticated);

  useEffect(() => {
    if (!qobuzStatusLoaded || !qobuzEnabled) return;
    markGettingStartedGuideComplete();
    setOpen(false);
  }, [qobuzEnabled, qobuzStatusLoaded]);

  const finish = (route?: RouteState) => {
    markGettingStartedGuideComplete();
    setOpen(false);
    if (route) onNavigate(route);
  };

  const chooseFolder = async () => {
    if (folderBusy) return;
    setFolderBusy(true);
    setFolderMessage('Opening the macOS folder picker…');
    try {
      const picked = await endpoints.pickFolder();
      const path = String(picked.path || '').trim();
      if (!path) {
        setFolderMessage('No folder selected.');
        return;
      }
      await endpoints.addFolder(path);
      setFolderAdded(true);
      setFolderMessage(`${path} is now linked to Fozmo.`);
    } catch (error) {
      setFolderMessage(error instanceof Error ? error.message : 'Could not add that folder.');
    } finally {
      setFolderBusy(false);
    }
  };

  return (
    <Modal
      open={open && qobuzStatusLoaded && !qobuzEnabled}
      className="metadata-assigner-backdrop getting-started-backdrop"
      ariaLabelledBy={titleId}
      onClose={() => finish()}
    >
      <section
        className="metadata-assigner-panel getting-started-modal app-modal-surface"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="metadata-assigner-head getting-started-head">
          <div>
            <strong id={titleId}>{stepTitle(step)}</strong>
            <span>
              Getting started · {step} of {STEP_COUNT}
            </span>
          </div>
          <button
            className="metadata-assigner-close"
            type="button"
            aria-label="Close getting started"
            onClick={() => finish()}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>

        <div className="metadata-assigner-body getting-started-body">
          {step === 1 ? (
            <>
              <p>Fozmo brings your local music and Qobuz library together in one player.</p>
              <div className="getting-started-options">
                <article className="getting-started-option">
                  <span className="getting-started-number">01</span>
                  <div>
                    <strong>Play your local library</strong>
                    <p>Choose a music folder on this Mac and Fozmo will add it to your library.</p>
                  </div>
                </article>
                <article className="getting-started-option">
                  <span className="getting-started-number">02</span>
                  <div>
                    <strong>Stream with Qobuz</strong>
                    <p>Connect your Qobuz account to browse and play its streaming catalogue.</p>
                  </div>
                </article>
              </div>
            </>
          ) : null}

          {step === 2 ? (
            <>
              <p>Fozmo can assign rich metadata and Qobuz links to albums in your local library.</p>
              <div className="getting-started-options">
                <article className="getting-started-option">
                  <span className="getting-started-number">A</span>
                  <div>
                    <strong>Automatically</strong>
                    <p>
                      Open <b>Settings → Metadata</b> and run <b>AutoMetadata</b> to match your
                      local albums.
                    </p>
                  </div>
                </article>
                <article className="getting-started-option">
                  <span className="getting-started-number">B</span>
                  <div>
                    <strong>Manually</strong>
                    <p>
                      Open an album, find its local release under <b>Versions</b>, then right-click
                      that version to assign metadata.
                    </p>
                  </div>
                </article>
              </div>
            </>
          ) : null}

          {step === 3 ? (
            <div className="getting-started-action-page">
              <p>
                Select the folder where your music is stored. The native macOS folder chooser will
                open on this Mac.
              </p>
              <button
                className="pill primary"
                type="button"
                disabled={folderBusy}
                onClick={() => void chooseFolder()}
              >
                {folderBusy ? 'Choosing…' : folderAdded ? 'Choose Another Folder' : 'Choose Folder'}
              </button>
              {folderMessage ? (
                <p className={`getting-started-message${folderAdded ? ' is-success' : ''}`}>
                  {folderMessage}
                </p>
              ) : null}
            </div>
          ) : null}

          {step === 4 ? (
            <div className="getting-started-action-page">
              <span className={`service-settings-status${qobuzConnected ? ' is-connected' : ''}`}>
                {qobuzStatus
                  ? qobuzAccountStatusLabel(qobuzStatus)
                  : 'Connection status unavailable'}
              </span>
              <p>Connect Qobuz to browse, stream and add Qobuz releases to your Fozmo library.</p>
              <a
                className="pill primary"
                href="/api/qobuz/oauth/start"
                onClick={() => markGettingStartedGuideComplete()}
              >
                Connect Qobuz
              </a>
            </div>
          ) : null}
        </div>

        <footer className="getting-started-foot">
          <div className="getting-started-dots" aria-label={`Step ${step} of ${STEP_COUNT}`}>
            {Array.from({ length: STEP_COUNT }, (_, index) => (
              <span key={index} className={step === index + 1 ? 'is-active' : ''} />
            ))}
          </div>
          <div className="spacer" />
          {step > 1 ? (
            <button className="pill" type="button" onClick={() => setStep((step - 1) as GuideStep)}>
              Back
            </button>
          ) : null}
          {step < STEP_COUNT ? (
            <button
              className="pill primary"
              type="button"
              onClick={() => setStep((step + 1) as GuideStep)}
            >
              Next
            </button>
          ) : (
            <button className="pill" type="button" onClick={() => finish()}>
              Finish
            </button>
          )}
        </footer>
      </section>
    </Modal>
  );
}
