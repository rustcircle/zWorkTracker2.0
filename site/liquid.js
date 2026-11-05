// A slow, pale liquid behind the page, drawn small and scaled up. It leans towards a mouse where
// one exists, and otherwise just drifts. Decorative only: no storage, no cookies, no requests.
// With JavaScript off, the page keeps the soft gradient set on html in styles.css.
(() => {
  const canvas = document.querySelector('.liquid');
  const ctx = canvas && canvas.getContext('2d', { alpha: false });
  if (!ctx) return;

  const still = matchMedia('(prefers-reduced-motion: reduce)');
  const dark = matchMedia('(prefers-color-scheme: dark)');
  const mouse = matchMedia('(pointer: fine)');
  const lowPower = !mouse.matches;
  const frameGap = 1000 / (lowPower ? 20 : 30);
  const STILL_TIME = 40000;
  let w = 0, h = 0, image, stops, running = false, last = 0;
  const p = { x: 0, y: 0, tx: 0, ty: 0, on: 0, onT: 0, ripple: 0 };

  const hex = (name) => {
    const v = getComputedStyle(document.documentElement).getPropertyValue(name).trim().slice(1);
    return [0, 2, 4].map((i) => parseInt(v.slice(i, i + 2), 16));
  };
  const readTheme = () => { stops = [hex('--bg'), hex('--liquid')]; };
  
  // Size from the canvas box, which CSS pins to the large viewport (100lvh), not from innerHeight:
  // iOS Safari fires resize every time its toolbar slides in or out while scrolling, and
  // re-allocating the canvas there clears it to black until the next frame. So only a real
  // change of the drawn size re-allocates, and the new canvas is painted at once.
  function resize() {
    const scale = lowPower ? 10 : 8;
    const nw = Math.max(32, Math.round(canvas.clientWidth / scale));
    const nh = Math.max(32, Math.round(canvas.clientHeight / scale));
    if (nw === w && nh === h) return false;
    w = nw; h = nh;
    canvas.width = w; canvas.height = h;
    image = ctx.createImageData(w, h);
    return true;
  }

  function draw(now) {
    const s = now / 1000 * .05;
    p.x += (p.tx - p.x) * .08; p.y += (p.ty - p.y) * .08;
    p.on += (p.onT - p.on) * .05;
    p.ripple *= .95;

    const [a, b] = stops, data = image.data;
    const unit = Math.min(w, h), fade = unit * unit * .12;
    const near = p.on > .01, ripple = p.ripple > .01, wave = now * .005;
    let i = 0;
    for (let y = 0; y < h; y++) {
      const vy = y / h;
      for (let x = 0; x < w; x++) {
        // Long diagonal bands, bent by a slower wave so they read as flow.
        const u = x / w, along = u * 2.2 + vy * 1.6;
        let phase = along * 2.4 + 1.5 * Math.sin((u - vy) * 2.6 + s * 1.3) + s * 2;
        if (near || ripple) {
          const dx = x - p.x, dy = y - p.y, d = dx * dx + dy * dy, e = Math.exp(-d / fade);
          phase += e * (p.on * 1.6 + (ripple ? p.ripple * 3 * Math.sin(Math.sqrt(d) * .6 - wave) : 0));
        }
        const c = .5 + .5 * Math.sin(phase);
        let t = c * c * (3 - 2 * c);
        // A little fixed dither stops banding across such a narrow colour range.
        t += ((x * 12.9898 + y * 78.233) * 43.758 % 1 - .5) * .06;
        t = Math.min(1, Math.max(0, t));
        // Every pixel mixes the two stops, so text contrast holds on any frame.
        data[i] = a[0] + (b[0] - a[0]) * t;
        data[i + 1] = a[1] + (b[1] - a[1]) * t;
        data[i + 2] = a[2] + (b[2] - a[2]) * t;
        data[i + 3] = 255;
        i += 4;
      }
    }
    ctx.putImageData(image, 0, 0);
  }

  function frame(now) {
    if (!running) return;
    if (now - last >= frameGap) { last = now; draw(now); }
    requestAnimationFrame(frame);
  }
  function update() {
    const go = !document.hidden && !still.matches;
    if (go && !running) { running = true; requestAnimationFrame(frame); }
    if (!go) running = false;
  }
  const paintStill = () => { if (!running) draw(STILL_TIME); };

  canvas.classList.add('on');
  readTheme(); resize(); draw(STILL_TIME);

  addEventListener('resize', () => { if (resize()) draw(running ? performance.now() : STILL_TIME); }, { passive: true });
  dark.addEventListener('change', () => { readTheme(); paintStill(); });
  still.addEventListener('change', () => { update(); paintStill(); });
  document.addEventListener('visibilitychange', update);

  if (mouse.matches) {
    addEventListener('pointermove', (e) => {
      if (e.pointerType !== 'mouse' || !running) return;
      const tx = e.clientX / innerWidth * w, ty = e.clientY / innerHeight * h;
      if (p.on < .01) { p.x = tx; p.y = ty; }
      p.onT = 1;
      p.ripple = Math.min(.3, p.ripple + Math.hypot(tx - p.tx, ty - p.ty) * .012);
      p.tx = tx; p.ty = ty;
    }, { passive: true });
    document.documentElement.addEventListener('pointerleave', () => { p.onT = 0; }, { passive: true });
  }
  update();
})();
