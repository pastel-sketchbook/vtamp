'use strict';
const $ = (id) => document.getElementById(id);
const gallery = $('gallery');
const slides = [...gallery.querySelectorAll('.gallery-slide')];
const selectors = [...gallery.querySelectorAll('[data-slide]')];
const rotation = $('gallery-rotation');
const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)');
const captions = [
  '120 × 28 cells. Your library and queue, together.',
  '100 × 24 cells. Less height. Still room for the cover and your library.',
  '100 × 24 cells. Press Tab to bring your queue alongside the music.'
];
let current = 0;
let automatic = !reducedMotion.matches;
let hovering = false;
let visible = true;
let timer;
const ready = new Set();

function renderRotation() {
  rotation.hidden = reducedMotion.matches;
  rotation.setAttribute('aria-label', automatic ? 'Pause automatic slideshow' : 'Start automatic slideshow');
  rotation.querySelector('span').textContent = automatic ? 'Pause' : 'Play';
  rotation.querySelector('path').setAttribute('d', automatic ? 'M8 5v14M16 5v14' : 'm8 5 11 7-11 7Z');
}
function schedule() {
  clearTimeout(timer);
  if (!automatic || hovering || !visible || document.hidden || reducedMotion.matches || ready.size < 2) return;
  timer = setTimeout(() => {
    let next = (current + 1) % slides.length;
    while (!ready.has(next)) next = (next + 1) % slides.length;
    show(next, false);
  }, 6000);
}
function show(index, manual) {
  if (!ready.has(index)) {
    $('gallery-status').textContent = 'This screenshot is still loading. Please try again.';
    return;
  }
  if (manual) {
    automatic = false;
    renderRotation();
    $('gallery-status').textContent = captions[index];
  }
  slides.forEach((slide, i) => { slide.hidden = i !== index; });
  selectors.forEach((button, i) => button.setAttribute('aria-pressed', String(i === index)));
  if (current !== index && !reducedMotion.matches) {
    slides[index].animate([{ opacity: 0 }, { opacity: 1 }], { duration: 220, easing: 'cubic-bezier(.16,1,.3,1)' });
  }
  current = index;
  $('gallery-description').textContent = captions[index];
  $('gallery-original').href = slides[index].querySelector('a').href;
  schedule();
}
selectors.forEach((button, index) => button.addEventListener('click', () => show(index, true)));
rotation.addEventListener('click', () => { automatic = !automatic; renderRotation(); schedule(); });
gallery.addEventListener('pointerenter', (event) => { if (event.pointerType === 'mouse') { hovering = true; schedule(); } });
gallery.addEventListener('pointerleave', () => { hovering = false; schedule(); });
// Keyboard navigation pauses rotation until the visitor explicitly starts it again.
gallery.addEventListener('focusin', (event) => {
  if (event.target.matches(':focus-visible') && !gallery.contains(event.relatedTarget)) { automatic = false; renderRotation(); schedule(); }
});
document.addEventListener('visibilitychange', schedule);
reducedMotion.addEventListener('change', () => {
  automatic = false;
  renderRotation();
  schedule();
});
if ('IntersectionObserver' in window) {
  new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; schedule(); }, { threshold: 0.2 }).observe(gallery);
}
async function loadSlide(index) {
  const image = slides[index].querySelector('img');
  if (image.dataset.src) image.src = image.dataset.src;
  try {
    await image.decode();
    ready.add(index);
    schedule();
  } catch {
    selectors[index].disabled = true;
    selectors[index].title = 'Screenshot unavailable';
    if (index === current) $('gallery-description').textContent = 'Screenshot unavailable. Please reload the page.';
  }
}
// Prioritize the first real screenshot, then fetch the other two.
loadSlide(0).then(() => { loadSlide(1); loadSlide(2); });
gallery.querySelector('.gallery-controls').hidden = false;
renderRotation();
let feedbackTimer;
document.querySelectorAll('[data-copy]').forEach((button) => {
  button.addEventListener('click', async () => {
    const command = button.dataset.copy;
    try { await navigator.clipboard.writeText(command); $('copy-feedback').textContent = 'Command copied.'; }
    catch { $('copy-feedback').textContent = `Select and copy: ${command}`; }
    clearTimeout(feedbackTimer);
    feedbackTimer = setTimeout(() => { $('copy-feedback').textContent = ''; }, 4500);
  });
});
