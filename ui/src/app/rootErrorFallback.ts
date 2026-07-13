export function renderRootErrorFallback(
  root: HTMLElement,
  reload: () => void = () => window.location.reload()
) {
  const main = document.createElement('main');
  main.className = 'react-app remote-auth-page';

  const panel = document.createElement('section');
  panel.className = 'remote-auth-panel';
  panel.setAttribute('role', 'alert');
  panel.setAttribute('aria-live', 'assertive');

  const kicker = document.createElement('div');
  kicker.className = 'remote-auth-kicker';
  kicker.textContent = 'Interface Error';

  const heading = document.createElement('h1');
  heading.textContent = 'Fozmo could not start';

  const detail = document.createElement('p');
  detail.textContent = 'Reload the app to recover from the unexpected interface error.';

  const actions = document.createElement('div');
  actions.className = 'remote-auth-actions';

  const reloadButton = document.createElement('button');
  reloadButton.className = 'pill primary';
  reloadButton.type = 'button';
  reloadButton.textContent = 'Reload Fozmo';
  reloadButton.addEventListener('click', reload);

  actions.append(reloadButton);
  panel.append(kicker, heading, detail, actions);
  main.append(panel);
  root.replaceChildren(main);
}
