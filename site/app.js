'use strict';
const tracks = [{ title: 'After Hours', seconds: 276 }, { title: 'Soft Focus', seconds: 232 }, { title: 'One More Commit', seconds: 308 }];
let current = 0, elapsed = 102, playing = true, attached = true, lastTick = performance.now();
const $ = (id) => document.getElementById(id);
const time = (value) => `${String(Math.floor(value / 60)).padStart(2, '0')}:${String(Math.floor(value % 60)).padStart(2, '0')}`;
function render() {
  $('elapsed').textContent = time(elapsed);
  $('server-time').textContent = time(elapsed);
  $('track-title').textContent = tracks[current].title;
  document.querySelector('.total').textContent = `/ ${time(tracks[current].seconds)}`;
  $('progress').style.width = `${Math.min(100, elapsed / tracks[current].seconds * 100)}%`;
  $('server-label').textContent = playing ? 'SERVER PLAYING' : 'SERVER PAUSED';
  $('demo-play').setAttribute('aria-label', playing ? 'Pause demo playback' : 'Resume demo playback');
  $('play-icon').setAttribute('d', playing ? 'M8 5v14M16 5v14' : 'm8 5 11 7-11 7Z');
  document.querySelector('.indicator').setAttribute('aria-label', playing ? 'Playing' : 'Paused');
  document.querySelectorAll('.demo-queue li').forEach((item, index) => item.classList.toggle('is-current', index === current));
}
function selectTrack(delta) { current = (current + delta + tracks.length) % tracks.length; elapsed = 0; playing = true; render(); }
$('demo-next').addEventListener('click', () => selectTrack(1));
$('demo-prev').addEventListener('click', () => selectTrack(-1));
$('demo-play').addEventListener('click', () => { playing = !playing; render(); });
$('detach').addEventListener('click', () => {
  attached = !attached;
  $('amp').hidden = !attached;
  $('detached').hidden = attached;
  $('detach').setAttribute('aria-expanded', String(attached));
  $('detach').firstChild.textContent = attached ? 'Detach interface ' : 'Attach again ';
  $('shell-command').textContent = attached ? 'vtamp' : 'vtamp → detached';
  document.querySelector('.terminal').classList.toggle('is-detached', !attached);
  $('demo-status').textContent = attached ? 'Attached to the same session. Interactive simulation; no browser audio.' : `Pane freed. The simulated server is still ${playing ? 'playing' : 'paused'}.`;
});
setInterval(() => {
  const now = performance.now();
  if (playing) { elapsed += (now - lastTick) / 1000; if (elapsed >= tracks[current].seconds) selectTrack(1); }
  lastTick = now; render();
}, 500);
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
render();
