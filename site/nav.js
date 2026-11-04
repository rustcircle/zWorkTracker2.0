// The phone menu: below 640px the main nav folds behind a button. With JavaScript off the nav
// stays a plain row of links (the .js class on html is what turns the fold on, see styles.css).
(() => {
  const bar = document.querySelector('.topbar');
  const button = bar && bar.querySelector('.nav-toggle');
  const nav = button && document.getElementById(button.getAttribute('aria-controls'));
  if (!nav) return;
  const phone = matchMedia('(max-width: 640px)');

  const isOpen = () => button.getAttribute('aria-expanded') === 'true';
  function set(open, focusButton) {
    button.setAttribute('aria-expanded', String(open));
    bar.classList.toggle('open', open);
    if (!open && focusButton) button.focus();
  }

  button.addEventListener('click', () => set(!isOpen()));
  // A link was chosen (in-page anchors included): fold the menu away.
  nav.addEventListener('click', (e) => { if (e.target.closest('a')) set(false); });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && isOpen()) { set(false, nav.contains(document.activeElement) || document.activeElement === button); }
  });
  // Clicking or tabbing anywhere outside the bar closes it.
  document.addEventListener('pointerdown', (e) => { if (isOpen() && !bar.contains(e.target)) set(false); });
  bar.addEventListener('focusout', (e) => { if (isOpen() && e.relatedTarget && !bar.contains(e.relatedTarget)) set(false); });
  // Widening past the phone breakpoint shows the normal row again.
  phone.addEventListener('change', () => { if (!phone.matches) set(false); });
})();
