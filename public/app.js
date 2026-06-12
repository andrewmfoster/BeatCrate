// ─── Navigation History ───────────────────────────────────────────────────────

const navHistory = [];
let navHistoryIndex = -1;
let isNavigatingHistory = false;

function pushHistory(entry) {
  if (isNavigatingHistory) return;
  navHistory.splice(navHistoryIndex + 1); // drop forward entries
  navHistory.push(entry);
  navHistoryIndex = navHistory.length - 1;
  updateNavArrows();
}

function updateNavArrows() {
  const back = document.getElementById('nav-back');
  const fwd  = document.getElementById('nav-forward');
  if (back) back.disabled = navHistoryIndex <= 0;
  if (fwd)  fwd.disabled  = navHistoryIndex >= navHistory.length - 1;
}

async function navigateBack() {
  if (navHistoryIndex <= 0) return;
  navHistoryIndex--;
  isNavigatingHistory = true;
  await replayHistoryEntry(navHistory[navHistoryIndex]);
  isNavigatingHistory = false;
  updateNavArrows();
}

async function navigateForward() {
  if (navHistoryIndex >= navHistory.length - 1) return;
  navHistoryIndex++;
  isNavigatingHistory = true;
  await replayHistoryEntry(navHistory[navHistoryIndex]);
  isNavigatingHistory = false;
  updateNavArrows();
}

async function replayHistoryEntry(entry) {
  if (entry.view === 'home') {
    await showHome();
  } else if (entry.view === 'grid') {
    if (entry.mode === 'all') await showAllCrates();
    else if (entry.mode === 'favorites') await showFavorites();
  } else if (entry.view === 'detail') {
    await openCrateDetail(entry.crateId);
  } else if (entry.view === 'career') {
    showCareerArc();
  }
}

// ─── State ───────────────────────────────────────────────────────────────────

const state = {
  profile: null,       // user profile { name, avatar_path }
  crates: [],          // all crates from API
  currentView: null,   // 'onboarding' | 'grid' | 'detail'
  gridMode: 'all',     // 'all' | 'favorites'
  activeCrate: null,   // crate object currently in detail view
  tracks: [],          // tracks for active crate (or flat list for fav/recent)
  playing: false,
  playingTrackId: null,  // id of the track currently loaded in the player
  playingQueue: [],      // queue belonging to the active playback context (never changed by navigation)
  playingIndex: -1,      // index into playingQueue
  playingCrate: null,    // crate for the active playback context (null for flat lists)
  inspectorOpen: false,  // whether the inspector panel is visible
  selectedTrackId: null, // track id currently shown in the inspector
  normalizationEnabled: false, // loaded from config on init
  pbNoteTrack: null,     // { id, title, notes } cache for the playing track (drives popover + badge)
  pbNotePopoverOpen: false,
};

// ─── Audio ───────────────────────────────────────────────────────────────────

// Web Audio API — gain node for volume control
const audioCtx = new (window.AudioContext || window.webkitAudioContext)();
const gainNode = audioCtx.createGain();
gainNode.gain.value = 1.0;
gainNode.connect(audioCtx.destination);

// Normalization gain node — sits upstream of gainNode, applies per-track replay_gain offset
const normGainNode = audioCtx.createGain();
normGainNode.gain.value = 1.0;
normGainNode.connect(gainNode);

// Buffer-based playback state
const audioBufferCache = new Map(); // trackId → AudioBuffer (pre-decoded)
// Decoded PCM is large (~63 MB per 3-min stereo track), so cap the cache. Insertion-order
// eviction over a long session, never evicting the track playing right now. The cache only
// needs the current track + immediate prefetch neighbours, so a small cap is plenty.
const AUDIO_CACHE_CAP = 16;
function cacheAudioBuffer(trackId, buffer) {
  audioBufferCache.set(trackId, buffer);
  if (audioBufferCache.size <= AUDIO_CACHE_CAP) return;
  for (const key of audioBufferCache.keys()) {
    if (audioBufferCache.size <= AUDIO_CACHE_CAP) break;
    if (key === state.playingTrackId) continue; // keep the live track warm
    audioBufferCache.delete(key);
  }
}
let currentAudioNode = null;        // active AudioBufferSourceNode
let pbStartTime   = 0;              // audioCtx.currentTime when current segment started
let pbStartOffset = 0;              // position in track when current segment started
let pbDuration    = 0;              // duration of currently loaded track
let pbTimeRAF     = null;           // requestAnimationFrame handle for progress updates

let isDraggingSeek = false;

// Drag-and-drop track reorder state
let dragSrcIndex  = null;
let dragOverIndex = null;
let dragOverPos   = null; // 'top' | 'bottom'

// Drag-and-drop todo reorder state
let todoDragSrc  = null;
let todoDragOver = null;
let todoDragPos  = null; // 'top' | 'bottom'

// Drag-and-drop note reorder state
let noteDragSrc  = null;
let noteDragOver = null;
let noteDragPos  = null; // 'top' | 'bottom'

// RAF-based progress update — runs while state.playing is true
function startPbTimeUpdate() {
  cancelAnimationFrame(pbTimeRAF);
  function tick() {
    if (!state.playing) return; // loop stops when paused/stopped
    if (!isDraggingSeek && pbDuration) {
      const pos = Math.min(pbStartOffset + (audioCtx.currentTime - pbStartTime), pbDuration);
      document.getElementById('pb-current').textContent = formatTime(pos);
      document.getElementById('pb-duration').textContent = '-' + formatTime(Math.max(0, pbDuration - pos));
      document.getElementById('pb-seek-fill').style.width = `${(pos / pbDuration) * 100}%`;
    }
    pbTimeRAF = requestAnimationFrame(tick);
  }
  pbTimeRAF = requestAnimationFrame(tick);
}

// Populate transport bar for a track without triggering audio playback
function cueTrackWithoutPlay(track, crate) {
  if (!track) return;
  state.playingTrackId = track.id;
  refreshPbNoteCache();
  pbStartOffset = 0;
  document.getElementById('pb-track-name').textContent = track.title;
  document.getElementById('pb-crate-name').textContent = crate ? crate.name : (track.crate_name || '');
  if (crate) setPbArt(crate);
  else if (track.crate_id) setPbArt({ id: track.crate_id });
  renderPbTags(track);
  updatePbStar();
  document.getElementById('pb-track-info').classList.toggle('active', !!crate || !!track.crate_id);
  // Use cached buffer duration if available, otherwise fall back to track metadata
  const cached = audioBufferCache.get(track.id);
  pbDuration = cached ? cached.duration : (track.duration || 0);
  document.getElementById('pb-duration').textContent = pbDuration ? '-' + formatTime(pbDuration) : '0:00';
  document.getElementById('pb-current').textContent = '0:00';
  document.getElementById('pb-seek-fill').style.width = '0%';
  refreshDetailPlayState();
}

function onTrackEnded() {
  if (repeatMode === 1) {
    // Repeat current song — replay from buffer cache
    const track = state.playingQueue[state.playingIndex];
    if (track) loadAndPlay(track, state.playingCrate);
  } else if (repeatMode === 2) {
    // Repeat album — wrap around to start when at the end
    const next = state.playingIndex + 1;
    if (next >= state.playingQueue.length) {
      state.playingIndex = 0;
      loadAndPlay(state.playingQueue[0], state.playingCrate);
    } else {
      skipTrack(1);
    }
  } else if (jukeboxActive) {
    // Jukebox shuffle — auto-advance to a new random track
    jukeboxSkip();
  } else {
    const next = state.playingIndex + 1;
    if (next >= state.playingQueue.length) {
      if (hwQueueActive) {
        // Last hw track finished — hand off to roulette
        hwQueueActive = false;
        jukeboxDice();
      } else {
        // Last track finished — stop completely, then cue the first track
        cancelAnimationFrame(pbTimeRAF);
        currentAudioNode = null;
        state.playing = false;
        document.getElementById('btn-play').textContent = '▶';
        setVinylSpin(false);
        // Without this, macOS Now Playing keeps showing ⏸ because the last
        // playbackState set was 'playing' from updateMediaSession().
        if ('mediaSession' in navigator) navigator.mediaSession.playbackState = 'paused';
        pauseMediaFocus();
        // Cue first track into transport bar without playing
        state.playingIndex = 0;
        cueTrackWithoutPlay(state.playingQueue[0], state.playingCrate);
        jukeboxSyncWidget();
      }
    } else {
      skipTrack(1);
    }
  }
}

// ─── Tauri bridge ────────────────────────────────────────────────────────────
//
// Every backend endpoint is a #[tauri::command]. api(path, opts) keeps a single
// fetch-style signature and translates (method, path, body) → invoke(command,
// args) via the route table below, so the ~60 call sites use one facade.
//
// Notes:
//  • Command args use camelCase keys; Tauri maps them to the snake_case Rust params.
//  • Returned wire shapes are explicit serde objects (0/1 ints stay ints), so
//    callers get stable, predictable values.
//  • invoke() returns the parsed value directly (no res.json()), and rejects with
//    the command's Err string — we rewrap it as an Error so callers' e.message works.

const { invoke, convertFileSrc } = window.__TAURI__.core;

// Native folder picker / reveal shim (window.beatcrateNative). Shimmed onto the
// Tauri dialog/opener plugins so the call sites stay unchanged.
// selectFolder() returns the chosen absolute path, or null if cancelled.
window.beatcrateNative = {
  async selectFolder(opts = {}) {
    const sel = await window.__TAURI__.dialog.open({
      directory: true,
      multiple: false,
      title: opts.title,
      defaultPath: opts.defaultPath,
    });
    return sel || null;
  },
  async revealPath(path) {
    try { await window.__TAURI__.opener.revealItemInDir(path); } catch (_) {}
  },
};

// Absolute on-disk cover path for a crate → an asset: URL the webview can load.
// crates.cover_path is already on each crate object in state.crates, so this is a
// synchronous lookup (works in the inline <img src> template strings). Returns ''
// when the crate has no cover, which makes <img>/Image() fire onerror (the old
// /api/crates/:id/cover route 404'd in the same case).
function crateCoverUrl(crateId) {
  const c = state.crates && state.crates.find(c => c.id === crateId);
  return c && c.cover_path ? convertFileSrc(c.cover_path) : '';
}

// Route table: [METHOD, pattern, command, mapArgs?]
//   pattern   '/api/…' with :params (':id'/':noteId' are coerced to Number)
//   mapArgs   (params, body, query) → args object for invoke (default: params)
// Literal routes are listed before their :param siblings so they win the match;
// method also disambiguates (e.g. PUT /todos/order vs PATCH /todos/:id).
const API_ROUTES = [
  ['GET',    '/api/config',                      'get_config'],
  ['POST',   '/api/config/ableton-root',         'set_ableton_root',      (_, b) => ({ path: b.path })],
  ['POST',   '/api/config/albums-folder',        'set_albums_folder',     (_, b) => ({ path: b.path })],
  ['POST',   '/api/config/rescan-albums',        'rescan_albums'],
  ['POST',   '/api/config/normalization',        'set_config_value',      (_, b) => ({ key: 'normalization_enabled',  value: String(b.enabled) })],
  ['POST',   '/api/config/library-mode',         'set_config_value',      (_, b) => ({ key: 'library_mode',           value: b.value })],
  ['POST',   '/api/config/crates-view',          'set_config_value',      (_, b) => ({ key: 'crates_view_mode',       value: b.value })],
  ['POST',   '/api/config/todo-collapsed',       'set_config_value',      (_, b) => ({ key: 'todo_collapsed',         value: b.value })],
  ['POST',   '/api/config/scratchpad-collapsed', 'set_config_value',      (_, b) => ({ key: 'scratchpad_collapsed',   value: b.value })],

  ['GET',    '/api/crates',                      'list_crates'],
  ['GET',    '/api/crates/:id/tracks',           'get_crate_tracks',      p => ({ id: p.id })],
  ['PUT',    '/api/crates/:id/tracks/order',     'set_crate_track_order', (p, b) => ({ id: p.id, order: b.order })],
  ['GET',    '/api/crates/:id/scratchpad',       'get_crate_scratchpad',  p => ({ id: p.id })],
  ['PUT',    '/api/crates/:id/scratchpad',       'set_crate_scratchpad',  (p, b) => ({ id: p.id, content: b.content })],
  ['PATCH',  '/api/crates/:id/status',           'set_crate_status',      (p, b) => ({ id: p.id, status: b.status })],
  ['PATCH',  '/api/crates/:id/producer',         'set_crate_producer',    (p, b) => ({ id: p.id, producer: b.producer })],
  ['GET',    '/api/crates/:id',                  'get_crate',             p => ({ id: p.id })],

  ['GET',    '/api/tracks/all',                  'list_all_tracks'],
  ['GET',    '/api/tracks/random',               'random_track'],
  ['GET',    '/api/tracks/analyze-status',       'analyze_status'],
  ['POST',   '/api/tracks/analyze-all',          'analyze_all'],
  ['POST',   '/api/tracks/:id/tags',             'add_track_tag',         (p, b) => ({ id: p.id, tag: b.tag })],
  ['DELETE', '/api/tracks/:id/tags/:tag',        'remove_track_tag',      p => ({ id: p.id, tag: p.tag })],
  ['GET',    '/api/tracks/:id/notes',            'get_track_notes',       p => ({ id: p.id })],
  ['POST',   '/api/tracks/:id/notes',            'add_track_note',        (p, b) => ({ id: p.id, note: b.note })],
  ['PUT',    '/api/tracks/:id/notes/order',      'set_track_notes_order', (p, b) => ({ id: p.id, items: b })],
  ['PATCH',  '/api/tracks/:id/notes/:noteId',    'update_track_note',     (p, b) => {
    const a = { id: p.id, noteId: p.noteId };
    if (b && b.note !== undefined)      a.note = b.note;
    if (b && b.completed !== undefined) a.completed = !!b.completed;
    return a;
  }],
  ['DELETE', '/api/tracks/:id/notes/:noteId',    'delete_track_note',     p => ({ id: p.id, noteId: p.noteId })],

  ['GET',    '/api/about',                       'get_about'],
  ['GET',    '/api/tags/track',                  'get_track_tags'],
  ['GET',    '/api/stats',                       'get_stats'],
  ['GET',    '/api/stats/weekly',                'get_weekly_stats'],
  ['GET',    '/api/stats/alltime',               'get_alltime_stats'],

  ['GET',    '/api/scratchpad',                  'get_scratchpad'],
  ['PUT',    '/api/scratchpad',                  'set_scratchpad',        (_, b) => ({ content: b.content })],

  ['GET',    '/api/todos',                       'get_todos'],
  ['POST',   '/api/todos',                       'add_todo',              (_, b) => ({ text: b.text })],
  ['PUT',    '/api/todos/order',                 'set_todos_order',       (_, b) => ({ items: b })],
  ['PATCH',  '/api/todos/:id/text',              'update_todo_text',      (p, b) => ({ id: p.id, text: b.text })],
  ['PATCH',  '/api/todos/:id',                   'set_todo_completed',    (p, b) => ({ id: p.id, completed: !!b.completed })],
  ['DELETE', '/api/todos/:id',                   'delete_todo',           p => ({ id: p.id })],

  ['GET',    '/api/profile',                     'get_profile'],
  ['PUT',    '/api/profile',                     'set_profile_name',      (_, b) => ({ name: b.name })],
  ['POST',   '/api/profile/avatar',              'set_profile_avatar',    (_, b) => ({ dataUrl: b.dataUrl })],

  ['GET',    '/api/search',                      'search',                (_, b, q) => ({ q: q.q })],

  ['GET',    '/api/insights/summary',            'insights_summary'],
  ['GET',    '/api/insights/timeline',           'insights_timeline'],
  ['GET',    '/api/insights/day-of-week',        'insights_day_of_week'],
  ['GET',    '/api/insights/plugins',            'insights_plugins'],
  ['POST',   '/api/insights/reindex',            'reindex_als'],
];

const COMPILED_ROUTES = API_ROUTES.map(([method, pattern, command, mapArgs]) => ({
  method, command, mapArgs, segs: pattern.split('/').filter(Boolean),
}));

function matchRoute(method, pathname) {
  const parts = pathname.split('/').filter(Boolean);
  for (const r of COMPILED_ROUTES) {
    if (r.method !== method || r.segs.length !== parts.length) continue;
    const params = {};
    let ok = true;
    for (let i = 0; i < r.segs.length; i++) {
      const s = r.segs[i];
      if (s[0] === ':') {
        const key = s.slice(1);
        let v = decodeURIComponent(parts[i]);
        if (key === 'id' || key === 'noteId') v = Number(v);
        params[key] = v;
      } else if (s !== parts[i]) { ok = false; break; }
    }
    if (ok) return { r, params };
  }
  return null;
}

async function api(path, opts = {}) {
  const method = (opts.method || 'GET').toUpperCase();
  const qIdx = path.indexOf('?');
  const pathname = qIdx === -1 ? path : path.slice(0, qIdx);
  const query = {};
  if (qIdx !== -1) new URLSearchParams(path.slice(qIdx + 1)).forEach((v, k) => { query[k] = v; });
  const body = opts.body !== undefined ? JSON.parse(opts.body) : undefined;

  const m = matchRoute(method, pathname);
  if (!m) throw new Error(`No Tauri command mapped for ${method} ${pathname}`);
  const args = m.r.mapArgs ? m.r.mapArgs(m.params, body, query) : m.params;
  try {
    return await invoke(m.r.command, args);
  } catch (err) {
    throw new Error(typeof err === 'string' ? err : (err && err.message) || 'command failed');
  }
}

// ─── Init ─────────────────────────────────────────────────────────────────────

async function init() {
  setupSettingsAutoSave();
  const config = await api('/api/config');
  state.normalizationEnabled = config.normalization_enabled === '1';
  const savedCratesView = config.crates_view_mode;
  cratesViewMode = CRATES_VIEW_MODES.includes(savedCratesView) ? savedCratesView : 'all';
  libraryMode = 'crates';
  todoCollapsed       = config.todo_collapsed       === '1';
  scratchpadCollapsed = config.scratchpad_collapsed === '1';
  if (!config.albums_folder) {
    showView('onboarding');
    injectMesh('onboarding-mesh-wrap');
    setupOnboarding();
    return;
  }
  await loadApp();
}

// ─── Aurora Mesh (animated WebGL — Vinyl Mesh Gradient) ──────────────────────

// Palette mapped to Honey: c0/bg + honey-tinted dark mid + accent + accent-hi
const MESH_PALETTE = {
  c0: [0.024, 0.039, 0.027],  // --bg #060a07
  c1: [0.100, 0.085, 0.055],  // honey-tinted dark wash
  c2: [0.784, 0.639, 0.353],  // --accent #c8a35a
  c3: [0.902, 0.769, 0.522],  // --accent-hi #e6c485
};

const MESH_CONFIG = {
  rotSpeed:    0.35,
  ripple:      0.42,
  grooveFreq:  104,
  edgeSoftness: 0.38,
  vinylSize:   0.79,
  vinylAmount: 1.0,
};

const MESH_VERT = `
  attribute vec2 aPos;
  varying vec2 vUv;
  void main(){
    vUv = aPos * 0.5 + 0.5;
    gl_Position = vec4(aPos, 0.0, 1.0);
  }
`;

const MESH_FRAG = `
  precision highp float;
  varying vec2 vUv;
  uniform vec2 uRes;
  uniform float uT;
  uniform vec3 uC0; uniform vec3 uC1; uniform vec3 uC2; uniform vec3 uC3;
  uniform float uRotSpeed;
  uniform float uRipple;
  uniform float uGrooveFreq;
  uniform float uSoftness;
  uniform float uVinylSize;
  uniform float uShowGrooves;

  vec2 hash2(vec2 p){
    p = vec2(dot(p, vec2(127.1,311.7)), dot(p, vec2(269.5,183.3)));
    return fract(sin(p) * 43758.5453) * 2.0 - 1.0;
  }
  float vnoise(vec2 p){
    vec2 i = floor(p); vec2 f = fract(p);
    vec2 u = f*f*(3.0-2.0*f);
    float a = dot(hash2(i+vec2(0.0,0.0)), f-vec2(0.0,0.0));
    float b = dot(hash2(i+vec2(1.0,0.0)), f-vec2(1.0,0.0));
    float c = dot(hash2(i+vec2(0.0,1.0)), f-vec2(0.0,1.0));
    float d = dot(hash2(i+vec2(1.0,1.0)), f-vec2(1.0,1.0));
    return mix(mix(a,b,u.x), mix(c,d,u.x), u.y);
  }
  float fbm(vec2 p){
    float v = 0.0; float a = 0.5;
    mat2 m = mat2(1.6, 1.2, -1.2, 1.6);
    for(int i=0; i<5; i++){ v += a * vnoise(p); p = m * p; a *= 0.5; }
    return v;
  }
  float meshField(vec2 p, float t){
    vec2 q = vec2(fbm(p + vec2(0.0, t*0.08)), fbm(p + vec2(5.2, -t*0.06) + 3.7));
    vec2 r = vec2(fbm(p + 1.7*q + vec2(1.7 + t*0.05, 9.2)), fbm(p + 1.7*q + vec2(8.3, 2.8 - t*0.04)));
    return fbm(p + 2.0*r);
  }
  vec3 palette(float v){
    v = clamp(v, 0.0, 1.0);
    vec3 col = mix(uC0, uC1, smoothstep(0.05, 0.42, v));
    col = mix(col, uC2, smoothstep(0.42, 0.72, v));
    col = mix(col, uC3, smoothstep(0.72, 0.92, v));
    col = mix(col, uC0 * 1.05, smoothstep(0.95, 1.0, v));
    return col;
  }
  void main(){
    vec2 uv = vUv;
    vec2 res = uRes;
    float aspect = res.x / res.y;
    vec2 p = (uv - 0.5);
    p.x *= aspect;
    float t = uT;
    vec2 mp = p * 1.7 + vec2(t*0.03, -t*0.02);
    float m = meshField(mp, t * 0.6);
    float mv = m * 0.62 + 0.5;
    vec2 vc = vec2(aspect * 0.5 + 0.18, -0.5 - 0.05);
    float vR = uVinylSize;
    vec2 d = p - vc;
    float r = length(d);
    float a = atan(d.y, d.x);
    float rotA = a + t * uRotSpeed;
    vec2 sp = vc + vec2(-vR * 0.62, vR * 0.62);
    float sd = length(p - sp);
    float ripplePhase = sd * 14.0 - t * 1.6;
    float rippleA = sin(ripplePhase) * exp(-sd * 1.2);
    float rippleB = sin(sd * 7.0 - t * 0.9 + 1.7) * exp(-sd * 0.7);
    float rippleAmt = (rippleA * 0.7 + rippleB * 0.45) * uRipple;
    float rr = r + rippleAmt * 0.04;
    float angWobble = sin(rotA * 3.0) * 0.004 + sin(rotA * 7.0 + 1.3) * 0.0018;
    float grooveR = rr + angWobble;
    float grooves = sin(grooveR * uGrooveFreq);
    float band = 0.5 + 0.5 * grooves;
    float discField = band * 0.65 + fbm(d * 3.0 + vec2(t*0.05, -t*0.03)) * 0.45 + 0.15;
    float labelMask = smoothstep(vR * 0.16, vR * 0.22, r);
    float edgeMask  = 1.0 - smoothstep(vR * 0.95, vR * 1.0, r);
    float wedge = pow(max(0.0, cos(rotA * 1.0)), 6.0) * 0.35
                + pow(max(0.0, cos(rotA * 1.0 + 3.14159)), 12.0) * 0.18;
    discField += wedge * labelMask * edgeMask;
    float discMask = (1.0 - smoothstep(vR * (1.0 - uSoftness), vR, r)) * uShowGrooves;
    float centerHole = smoothstep(vR * 0.05, vR * 0.18, r);
    discMask *= centerHole;
    float v = mix(mv, mix(mv, discField, 0.92), discMask);
    float grain = (fract(sin(dot(uv * res, vec2(12.9898, 78.233))) * 43758.5453) - 0.5) * 0.018;
    v += grain;
    vec3 col = palette(v);
    col += vec3(0.18, 0.12, 0.05) * wedge * discMask * 0.8;
    float vig = smoothstep(1.25, 0.35, length((uv - 0.5) * vec2(aspect, 1.0)));
    col *= mix(0.82, 1.0, vig);
    col = pow(col, vec3(0.95));
    gl_FragColor = vec4(col, 1.0);
  }
`;

const _meshCanvases = [];  // [{ canvas, gl, prog, uniforms, wrap }]
let _meshRafStarted = false;

function _compileMeshShader(gl, type, src) {
  const s = gl.createShader(type);
  gl.shaderSource(s, src);
  gl.compileShader(s);
  if (!gl.getShaderParameter(s, gl.COMPILE_STATUS)) {
    console.error('mesh shader compile error:', gl.getShaderInfoLog(s));
    gl.deleteShader(s);
    return null;
  }
  return s;
}

function injectMesh(wrapId) {
  const wrap = document.getElementById(wrapId);
  if (!wrap) return;
  wrap.innerHTML = '';

  const canvas = document.createElement('canvas');
  wrap.appendChild(canvas);

  const gl = canvas.getContext('webgl', { antialias: false, premultipliedAlpha: false });
  if (!gl) {
    // Fallback: tint the wrap with a static gradient so views aren't pitch black if WebGL is unavailable
    wrap.style.background = 'radial-gradient(ellipse at 78% 60%, rgba(200,163,90,0.18), transparent 65%), var(--bg)';
    return;
  }

  const vs = _compileMeshShader(gl, gl.VERTEX_SHADER, MESH_VERT);
  const fs = _compileMeshShader(gl, gl.FRAGMENT_SHADER, MESH_FRAG);
  if (!vs || !fs) return;

  const prog = gl.createProgram();
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    console.error('mesh program link error:', gl.getProgramInfoLog(prog));
    return;
  }
  gl.useProgram(prog);

  const aPos = gl.getAttribLocation(prog, 'aPos');
  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1, 3,-1, -1,3]), gl.STATIC_DRAW);
  gl.enableVertexAttribArray(aPos);
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);

  const uniforms = {
    uRes:         gl.getUniformLocation(prog, 'uRes'),
    uT:           gl.getUniformLocation(prog, 'uT'),
    uC0:          gl.getUniformLocation(prog, 'uC0'),
    uC1:          gl.getUniformLocation(prog, 'uC1'),
    uC2:          gl.getUniformLocation(prog, 'uC2'),
    uC3:          gl.getUniformLocation(prog, 'uC3'),
    uRotSpeed:    gl.getUniformLocation(prog, 'uRotSpeed'),
    uRipple:      gl.getUniformLocation(prog, 'uRipple'),
    uGrooveFreq:  gl.getUniformLocation(prog, 'uGrooveFreq'),
    uSoftness:    gl.getUniformLocation(prog, 'uSoftness'),
    uVinylSize:   gl.getUniformLocation(prog, 'uVinylSize'),
    uShowGrooves: gl.getUniformLocation(prog, 'uShowGrooves'),
  };

  _meshCanvases.push({ canvas, gl, prog, uniforms, wrap });
  _startMeshRAF();
}

function _isMeshWrapActive(wrap) {
  // Wrap is inside a .view. The view's parent toggles .active to show it.
  const view = wrap.closest('.view');
  return !!(view && view.classList.contains('active'));
}

function _startMeshRAF() {
  if (_meshRafStarted) return;
  _meshRafStarted = true;
  const t0 = performance.now();
  function frame(now) {
    const elapsed = (now - t0) / 1000;
    for (const ctx of _meshCanvases) {
      if (!_isMeshWrapActive(ctx.wrap)) continue;
      const dpr = Math.min(window.devicePixelRatio || 1, 1.8);
      const cw = Math.max(1, Math.floor(ctx.canvas.clientWidth  * dpr));
      const ch = Math.max(1, Math.floor(ctx.canvas.clientHeight * dpr));
      if (ctx.canvas.width !== cw || ctx.canvas.height !== ch) {
        ctx.canvas.width = cw; ctx.canvas.height = ch;
      }
      const gl = ctx.gl;
      gl.useProgram(ctx.prog);
      gl.viewport(0, 0, cw, ch);
      gl.uniform2f(ctx.uniforms.uRes, cw, ch);
      gl.uniform1f(ctx.uniforms.uT, elapsed);
      gl.uniform3fv(ctx.uniforms.uC0, MESH_PALETTE.c0);
      gl.uniform3fv(ctx.uniforms.uC1, MESH_PALETTE.c1);
      gl.uniform3fv(ctx.uniforms.uC2, MESH_PALETTE.c2);
      gl.uniform3fv(ctx.uniforms.uC3, MESH_PALETTE.c3);
      gl.uniform1f(ctx.uniforms.uRotSpeed,    MESH_CONFIG.rotSpeed);
      gl.uniform1f(ctx.uniforms.uRipple,      MESH_CONFIG.ripple);
      gl.uniform1f(ctx.uniforms.uGrooveFreq,  MESH_CONFIG.grooveFreq);
      gl.uniform1f(ctx.uniforms.uSoftness,    MESH_CONFIG.edgeSoftness);
      gl.uniform1f(ctx.uniforms.uVinylSize,   MESH_CONFIG.vinylSize);
      gl.uniform1f(ctx.uniforms.uShowGrooves, MESH_CONFIG.vinylAmount);
      gl.drawArrays(gl.TRIANGLES, 0, 3);
    }
    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

async function loadApp() {
  state.profile = await fetchProfile();
  renderTitlebarAvatar();
  if (!state.profile.name) {
    showProfileOnboarding();
    return;
  }
  state.crates = await api('/api/crates');
  injectMesh('home-mesh-wrap');
  injectMesh('grid-mesh-wrap');
  injectMesh('career-mesh-wrap');
  updatePbNoteBadge();
  document.addEventListener('click', (e) => {
    const pop = document.getElementById('songs-tag-popover');
    if (!pop || pop.style.display === 'none') return;
    // Bail out if click was inside the tag-filter wrap (toggle button, popover, or any
    // pill). `closest` works on detached nodes too — toggleSongsTagFilter re-renders the
    // popover, so by the time this handler fires e.target is detached but its parent
    // chain (pill → pills → popover → wrap) is intact in the detached subtree.
    if (e.target.closest && e.target.closest('.songs-tag-filter-wrap')) return;
    pop.style.display = 'none';
  });
  showHome();
}

// ─── Onboarding ──────────────────────────────────────────────────────────────

function setupOnboarding() {
  const btn = document.getElementById('onboarding-btn');
  const pathEl = document.getElementById('onboarding-path');
  const input = document.getElementById('onboarding-path-input');
  const isNative = !!window.beatcrateNative;

  if (isNative) {
    // Native shim present: button opens the native folder dialog directly.
    btn.onclick = async () => {
      const picked = await window.beatcrateNative.selectFolder({
        title: 'Choose Your Albums Folder',
        buttonLabel: 'Use This Folder',
      });
      if (!picked) return;
      await commitOnboardingFolder(picked, btn, pathEl);
    };
  } else {
    // Browser fallback: show a text input the user can paste/type into.
    // There's no way to get an absolute path from the browser's webkitdirectory
    // picker, and this dev-only path doesn't need to be pretty.
    input.style.display = '';
    btn.textContent = 'Use This Folder';
    btn.onclick = async () => {
      const fullPath = input.value.trim();
      if (!fullPath) { input.focus(); return; }
      await commitOnboardingFolder(fullPath, btn, pathEl);
    };
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter') btn.click();
    });
  }
}

async function commitOnboardingFolder(fullPath, btn, pathEl) {
  const originalLabel = btn.textContent;
  btn.textContent = 'Loading…';
  btn.disabled = true;
  pathEl.textContent = fullPath;
  try {
    await api('/api/config/albums-folder', {
      method: 'POST',
      body: JSON.stringify({ path: fullPath }),
    });
    await loadApp();
  } catch (err) {
    pathEl.textContent = `Error: ${err.message}`;
    btn.textContent = originalLabel;
    btn.disabled = false;
  }
}

// ─── Avatar helpers ───────────────────────────────────────────────────────────

function getInitials(name) {
  if (!name) return '';
  return name.trim().split(/\s+/).map(w => w[0]).join('').toUpperCase().slice(0, 2);
}

// Fetch the profile and resolve its stored "/uploads/<file>" avatar reference into
// an asset: URL the webview can render. avatar_url is the loadable one.
async function fetchProfile() {
  const p = await api('/api/profile');
  if (p && p.avatar_path) {
    try { p.avatar_url = convertFileSrc(await invoke('avatar_path', { filename: p.avatar_path })); }
    catch (_) {}
  }
  return p;
}

// Renders profile photo (or initials fallback) into any avatar container element.
// Pass editMode: true for edit circles that should show a "+" via CSS instead of initials.
function renderAvatarInEl(el, profile, { editMode = false } = {}) {
  if (profile && profile.avatar_url) {
    el.innerHTML = `<img src="${escHtml(profile.avatar_url)}" alt="${escHtml(profile.name || '')}">`;
    el.classList.add('has-photo');
  } else {
    el.classList.remove('has-photo');
    if (!editMode) {
      const initials = getInitials(profile ? profile.name : '');
      el.innerHTML = initials ? `<span>${escHtml(initials)}</span>` : '';
    } else {
      el.innerHTML = '';
    }
  }
}

function renderTitlebarAvatar() {
  const el = document.getElementById('titlebar-avatar');
  if (el) renderAvatarInEl(el, state.profile);
}

// ─── Avatar resize ────────────────────────────────────────────────────────────

function resizeImageToDataUrl(dataUrl, maxSize) {
  return new Promise(resolve => {
    const img = new Image();
    img.onload = () => {
      let { width, height } = img;
      if (width > maxSize || height > maxSize) {
        if (width >= height) {
          height = Math.round(height * maxSize / width);
          width = maxSize;
        } else {
          width = Math.round(width * maxSize / height);
          height = maxSize;
        }
      }
      const canvas = document.createElement('canvas');
      canvas.width = width;
      canvas.height = height;
      canvas.getContext('2d').drawImage(img, 0, 0, width, height);
      resolve(canvas.toDataURL('image/jpeg', 0.85));
    };
    img.src = dataUrl;
  });
}

// ─── Profile onboarding ───────────────────────────────────────────────────────

let pendingObAvatarDataUrl = null;

function showProfileOnboarding() {
  pendingObAvatarDataUrl = null;
  showView('profile-onboarding');
  injectMesh('profile-ob-mesh-wrap');
  // Re-set preview to blank + re-attach file input listener
  const preview = document.getElementById('profile-ob-avatar-preview');
  if (preview) {
    preview.innerHTML = '<span class="profile-ob-avatar-plus">+</span>';
    preview.classList.remove('has-photo');
  }
  const nameInput = document.getElementById('profile-ob-name');
  if (nameInput) { nameInput.value = ''; nameInput.classList.remove('error'); }
  const btn = document.getElementById('profile-ob-btn');
  if (btn) { btn.disabled = false; btn.textContent = 'Get Started'; }

  const fileInput = document.getElementById('profile-ob-file');
  const fresh = fileInput.cloneNode(true);
  fileInput.parentNode.replaceChild(fresh, fileInput);
  fresh.addEventListener('change', () => {
    const file = fresh.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = async e => {
      const resized = await resizeImageToDataUrl(e.target.result, 400);
      pendingObAvatarDataUrl = resized;
      const prev = document.getElementById('profile-ob-avatar-preview');
      if (prev) {
        prev.innerHTML = `<img src="${escHtml(resized)}" alt="">`;
        prev.classList.add('has-photo');
      }
    };
    reader.readAsDataURL(file);
  });

  setTimeout(() => nameInput && nameInput.focus(), 80);
}

async function finishProfileOnboarding() {
  const nameInput = document.getElementById('profile-ob-name');
  const name = nameInput ? nameInput.value.trim() : '';
  if (!name) {
    if (nameInput) { nameInput.focus(); nameInput.classList.add('error'); }
    return;
  }

  const btn = document.getElementById('profile-ob-btn');
  if (btn) { btn.disabled = true; btn.textContent = 'Saving…'; }

  try {
    await api('/api/profile', { method: 'PUT', body: JSON.stringify({ name }) });
    if (pendingObAvatarDataUrl) {
      await api('/api/profile/avatar', { method: 'POST', body: JSON.stringify({ dataUrl: pendingObAvatarDataUrl }) });
    }
    // Hand back to loadApp() so the full init path runs (mesh injection for
    // home/grid/career, click handlers, profile fetch, crate fetch, showHome).
    // Without this, first-launch sessions land on Home with no mesh.
    await loadApp();
  } catch (e) {
    if (btn) { btn.disabled = false; btn.textContent = 'Get Started'; }
  }
}

// ─── Settings ─────────────────────────────────────────────────────────────────

async function showSettings() {
  const popover = document.getElementById('settings-popover');
  if (popover.classList.contains('open')) {
    closeSettings();
    return;
  }

  // Browse buttons only work when the native shim (window.beatcrateNative) is
  // present. Otherwise hide them so users aren't presented with a
  // non-functional control.
  const isNative = !!window.beatcrateNative;
  ['settings-browse-folder-btn', 'settings-browse-ableton-btn'].forEach(id => {
    const el = document.getElementById(id);
    if (el) el.style.display = isNative ? '' : 'none';
  });

  const nameInput = document.getElementById('settings-name-input');
  if (nameInput) nameInput.value = state.profile ? (state.profile.name || '') : '';

  const avatarEdit = document.getElementById('settings-avatar-edit');
  if (avatarEdit) renderAvatarInEl(avatarEdit, state.profile, { editMode: true });

  const pStatus = document.getElementById('settings-profile-status');
  const fStatus = document.getElementById('settings-folder-status');
  const aStatus = document.getElementById('settings-ableton-status');
  const aReindexStatus = document.getElementById('settings-reindex-status');
  const rescanStatus = document.getElementById('settings-rescan-albums-status');
  if (pStatus) { pStatus.textContent = ''; pStatus.className = 'settings-save-status'; }
  if (fStatus) { fStatus.textContent = ''; fStatus.className = 'settings-save-status'; }
  if (aStatus) { aStatus.textContent = ''; aStatus.className = 'settings-save-status'; }
  if (aReindexStatus) { aReindexStatus.textContent = ''; aReindexStatus.className = 'settings-save-status'; }
  if (rescanStatus) { rescanStatus.textContent = ''; rescanStatus.className = 'settings-save-status'; }

  // About section — version + data dir. Hide reveal-in-finder when the native
  // shim isn't present (only it can reveal a path in Finder).
  try {
    const about = await api('/api/about');
    const vEl = document.getElementById('settings-about-version');
    const dEl = document.getElementById('settings-about-data-dir');
    if (vEl) vEl.textContent = about.version ? `v${about.version}` : '—';
    if (dEl) dEl.textContent = about.dataDir || '—';
  } catch (e) {}
  const revealBtn = document.getElementById('settings-reveal-data-btn');
  if (revealBtn) revealBtn.style.display = isNative ? '' : 'none';

  try {
    const config = await api('/api/config');
    const folderInput = document.getElementById('settings-folder-input');
    if (folderInput) folderInput.value = config.albums_folder || '';
    const abletonInput = document.getElementById('settings-ableton-input');
    if (abletonInput) abletonInput.value = config.ableton_root || '';
    const normToggle = document.getElementById('normalization-toggle');
    if (normToggle) normToggle.checked = config.normalization_enabled === '1';
    const normStatus = document.getElementById('normalization-status');
    if (normStatus) { normStatus.textContent = ''; normStatus.className = 'settings-save-status'; }
  } catch (e) {}

  const fileInput = document.getElementById('settings-avatar-file');
  const fresh = fileInput.cloneNode(true);
  fileInput.parentNode.replaceChild(fresh, fileInput);
  fresh.addEventListener('change', () => {
    const file = fresh.files[0];
    if (!file) return;
    const statusEl = document.getElementById('settings-profile-status');
    const reader = new FileReader();
    reader.onerror = () => {
      flashSettingsStatus(statusEl, 'Could not read photo file.', 'err');
      fresh.value = '';
    };
    reader.onload = async e => {
      const resized = await resizeImageToDataUrl(e.target.result, 400);
      const el = document.getElementById('settings-avatar-edit');
      if (el) {
        el.innerHTML = `<img src="${escHtml(resized)}" alt="">`;
        el.classList.add('has-photo');
      }
      flashSettingsStatus(statusEl, 'Saving…', 'pending');
      try {
        await api('/api/profile/avatar', { method: 'POST', body: JSON.stringify({ dataUrl: resized }) });
        state.profile = await fetchProfile();
        renderTitlebarAvatar();
        flashSettingsStatus(statusEl, 'Photo updated.', 'ok');
      } catch (err) {
        console.error('avatar save failed:', err);
        flashSettingsStatus(statusEl, `Could not save photo (${err.message}).`, 'err');
      } finally {
        // Reset so re-picking the same file still fires `change`.
        fresh.value = '';
      }
    };
    reader.readAsDataURL(file);
  });

  popover.classList.add('open');
  popover.setAttribute('aria-hidden', 'false');
}

function closeSettings() {
  const popover = document.getElementById('settings-popover');
  popover.classList.remove('open');
  popover.setAttribute('aria-hidden', 'true');
}

// Status helper for auto-save flow. "ok" fades after a beat; "err" stays put.
// "pending" (the "Saving…" interstitial) clears as soon as the real result arrives.
function flashSettingsStatus(statusEl, message, kind) {
  if (!statusEl) return;
  clearTimeout(statusEl._fadeTimer);
  statusEl.textContent = message;
  statusEl.className = `settings-save-status${kind === 'ok' ? ' ok' : kind === 'err' ? ' err' : ''}`;
  if (kind === 'ok') {
    statusEl._fadeTimer = setTimeout(() => {
      statusEl.textContent = '';
      statusEl.className = 'settings-save-status';
    }, 2500);
  }
}

// ─── Global toast (auto-dismiss banner) ──────────────────────────────────────
// The app has no other general notification surface; this is where backend and
// renderer failures get told to the user. Banner slides in bottom-right and
// auto-fades after ~4.2s; a click dismisses it early. kind: 'info' | 'err'.
function showToast(message, kind = 'info') {
  const stack = document.getElementById('toast-stack');
  if (!stack || !message) return;
  const el = document.createElement('div');
  el.className = kind === 'err' ? 'toast err' : 'toast';
  const icon = document.createElement('span');
  icon.className = 'toast-icon';
  icon.textContent = kind === 'err' ? '⚠' : '✓';
  const msg = document.createElement('span');
  msg.className = 'toast-msg';
  msg.textContent = message;
  el.append(icon, msg);
  stack.appendChild(el);
  requestAnimationFrame(() => el.classList.add('show'));
  const remove = () => {
    clearTimeout(el._timer);
    el.classList.remove('show');
    setTimeout(() => el.remove(), 320);
  };
  el._timer = setTimeout(remove, 4200);
  el.addEventListener('click', remove);
}

// Backend → renderer notices surface as toasts. The Rust side (Batch 1c) emits
// `beatcrate-notice` with { message, kind } when a background op (re-ingest,
// .als index, loudness analysis) fails out of band. No-op if the event API or
// the emit isn't present, so this is safe to wire before any backend support.
async function setupBackendNoticeListener() {
  try {
    const ev = window.__TAURI__ && window.__TAURI__.event;
    if (!ev || !ev.listen) return;
    await ev.listen('beatcrate-notice', e => {
      const p = (e && e.payload) || {};
      showToast(p.message || 'Something went wrong.', p.kind === 'err' ? 'err' : 'info');
    });
  } catch (err) {
    console.error('notice listener setup failed:', err);
  }
}

// Wires blur + Enter on a settings input so changes commit without a Save button.
// `saveFn(trimmedValue, statusEl)` is only called when the value actually changed.
function wireAutoSaveInput(inputId, statusElId, saveFn) {
  const input = document.getElementById(inputId);
  if (!input || input._autoSaveWired) return;
  input._autoSaveWired = true;
  input.addEventListener('focus', () => { input._prevValue = input.value; });
  input.addEventListener('blur', () => {
    const next = input.value.trim();
    const prev = (input._prevValue || '').trim();
    if (next === prev) return;
    input._prevValue = next;
    saveFn(next, document.getElementById(statusElId));
  });
  input.addEventListener('keydown', e => {
    if (e.key === 'Enter') { e.preventDefault(); input.blur(); }
  });
}

function setupSettingsAutoSave() {
  wireAutoSaveInput('settings-name-input', 'settings-profile-status', saveProfileName);
  wireAutoSaveInput('settings-folder-input', 'settings-folder-status', saveMusicFolder);
  wireAutoSaveInput('settings-ableton-input', 'settings-ableton-status', saveAbletonFolder);
}

async function saveProfileName(name, statusEl) {
  flashSettingsStatus(statusEl, 'Saving…', 'pending');
  try {
    await api('/api/profile', { method: 'PUT', body: JSON.stringify({ name }) });
    state.profile = await fetchProfile();
    renderTitlebarAvatar();
    flashSettingsStatus(statusEl, 'Saved.', 'ok');
  } catch (e) {
    console.error('saveProfileName error:', e);
    flashSettingsStatus(statusEl, 'Could not save.', 'err');
  }
}

async function saveMusicFolder(folderPath, statusEl) {
  if (!folderPath) {
    flashSettingsStatus(statusEl, 'Enter a folder path.', 'err');
    return;
  }
  flashSettingsStatus(statusEl, 'Saving…', 'pending');
  try {
    await api('/api/config/albums-folder', { method: 'POST', body: JSON.stringify({ path: folderPath }) });
    state.crates = await api('/api/crates');
    flashSettingsStatus(statusEl, 'Folder saved. Library updated.', 'ok');
  } catch (e) {
    flashSettingsStatus(statusEl, 'Could not save folder.', 'err');
  }
}

async function saveAbletonFolder(folderPath, statusEl) {
  if (!folderPath) {
    flashSettingsStatus(statusEl, 'Enter a folder path.', 'err');
    return;
  }
  flashSettingsStatus(statusEl, 'Saving…', 'pending');
  try {
    await api('/api/config/ableton-root', { method: 'POST', body: JSON.stringify({ path: folderPath }) });
    flashSettingsStatus(statusEl, 'Folder saved.', 'ok');
  } catch (e) {
    flashSettingsStatus(statusEl, 'Could not save folder.', 'err');
  }
}

async function browseAlbumsFolder() {
  if (!window.beatcrateNative) return;
  const input = document.getElementById('settings-folder-input');
  const current = input ? input.value.trim() : '';
  const picked = await window.beatcrateNative.selectFolder({
    title: 'Select Music Folder',
    buttonLabel: 'Use This Folder',
    defaultPath: current || undefined,
  });
  if (!picked || !input) return;
  input.value = picked;
  input._prevValue = picked;
  await saveMusicFolder(picked, document.getElementById('settings-folder-status'));
}

async function browseAbletonFolder() {
  if (!window.beatcrateNative) return;
  const input = document.getElementById('settings-ableton-input');
  const current = input ? input.value.trim() : '';
  const picked = await window.beatcrateNative.selectFolder({
    title: 'Select Ableton Projects Folder',
    buttonLabel: 'Use This Folder',
    defaultPath: current || undefined,
  });
  if (!picked || !input) return;
  input.value = picked;
  input._prevValue = picked;
  await saveAbletonFolder(picked, document.getElementById('settings-ableton-status'));
}

async function rescanAlbums() {
  const statusEl = document.getElementById('settings-rescan-albums-status');
  const btn = document.getElementById('settings-rescan-albums-btn');

  if (btn) { btn.disabled = true; btn.textContent = 'Scanning…'; }
  if (statusEl) { statusEl.textContent = ''; statusEl.className = 'settings-save-status'; }

  try {
    const result = await api('/api/config/rescan-albums', { method: 'POST' });
    // Refresh in-memory crates so the Library reflects any new content immediately.
    state.crates = await api('/api/crates');
    const msg = `Scanned ${result.crates} crate${result.crates !== 1 ? 's' : ''}, ${result.tracks} track${result.tracks !== 1 ? 's' : ''}.`;
    if (statusEl) { statusEl.textContent = msg; statusEl.className = 'settings-save-status ok'; }
  } catch (e) {
    const msg = e && e.message ? e.message : 'Re-scan failed.';
    if (statusEl) { statusEl.textContent = msg; statusEl.className = 'settings-save-status err'; }
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = 'Re-scan Library'; }
  }
}

async function revealDataFolder() {
  if (!window.beatcrateNative) return;
  const dEl = document.getElementById('settings-about-data-dir');
  const dataDir = dEl ? dEl.textContent.trim() : '';
  if (!dataDir || dataDir === '—') return;
  await window.beatcrateNative.revealPath(dataDir);
}

async function reindexSessions() {
  const statusEl = document.getElementById('settings-reindex-status');
  const btn = document.getElementById('settings-reindex-sessions-btn');

  if (btn) { btn.disabled = true; btn.textContent = 'Scanning…'; }
  if (statusEl) { statusEl.textContent = ''; statusEl.className = 'settings-save-status'; }

  try {
    const result = await api('/api/insights/reindex', { method: 'POST' });
    let msg = `Scanned ${result.scanned} session${result.scanned !== 1 ? 's' : ''}`;
    if (result.failed > 0) msg += `, ${result.failed} failed`;
    if (result.pruned > 0) msg += `, removed ${result.pruned} deleted`;
    if (statusEl) { statusEl.textContent = msg; statusEl.className = 'settings-save-status ok'; }
    // M10: name the projects that wouldn't parse so they're actionable, not just a count.
    const failedFiles = result.failed_files || [];
    if (failedFiles.length) {
      const shown = failedFiles.slice(0, 3).join(', ');
      const more = failedFiles.length > 3 ? ` +${failedFiles.length - 3} more` : '';
      showToast(`${failedFiles.length} Ableton project${failedFiles.length !== 1 ? 's' : ''} couldn't be read: ${shown}${more}`, 'err');
    }
  } catch (e) {
    const msg = e && e.message ? e.message : 'Ableton Projects Folder is not configured.';
    if (statusEl) { statusEl.textContent = msg; statusEl.className = 'settings-save-status err'; }
  } finally {
    if (btn) { btn.disabled = false; btn.textContent = 'Re-scan Sessions'; }
  }
}

async function toggleNormalization() {
  const toggle = document.getElementById('normalization-toggle');
  const statusEl = document.getElementById('normalization-status');
  const enabled = toggle.checked;

  try {
    await api('/api/config/normalization', { method: 'POST', body: JSON.stringify({ enabled: enabled ? 1 : 0 }) });
    state.normalizationEnabled = enabled;

    // Update gain on the currently playing track immediately
    if (state.playingTrackId) {
      const currentTrack = state.playingQueue.find(t => t.id === state.playingTrackId);
      applyNormGain(currentTrack || null);
    } else {
      normGainNode.gain.value = 1.0;
    }

    if (!enabled) {
      statusEl.textContent = 'Normalization disabled.';
      statusEl.className = 'settings-save-status';
      return;
    }

    // Kick off analysis for any tracks that haven't been analyzed yet
    const result = await api('/api/tracks/analyze-all', { method: 'POST' });

    if (result.done) {
      statusEl.textContent = result.total === 0
        ? 'All tracks already analyzed.'
        : `Analysis complete — ${result.total} tracks normalized.`;
      statusEl.className = 'settings-save-status ok';
      return;
    }

    statusEl.textContent = `Analyzing ${result.total} tracks…`;
    statusEl.className = 'settings-save-status';

    // Poll for progress every 1.5 s
    const pollId = setInterval(async () => {
      try {
        const job = await api('/api/tracks/analyze-status');
        if (job.done) {
          clearInterval(pollId);
          // H2: job.completed now counts only tracks whose gain was actually
          // stored; note any that failed so the count isn't silently short.
          statusEl.textContent = job.failed > 0
            ? `Analysis complete — ${job.completed} normalized, ${job.failed} failed.`
            : `Analysis complete — ${job.completed} tracks normalized.`;
          statusEl.className = 'settings-save-status ok';
        } else {
          statusEl.textContent = `Analyzing tracks… ${job.completed} / ${job.total}`;
        }
      } catch (_) {}
    }, 1500);
  } catch (e) {
    statusEl.textContent = 'Could not save setting.';
    statusEl.className = 'settings-save-status err';
    toggle.checked = !enabled; // revert toggle on error
  }
}

// ─── Views ───────────────────────────────────────────────────────────────────

function showView(name) {
  document.querySelectorAll('.view').forEach(v => v.classList.remove('active'));
  document.getElementById(`view-${name}`).classList.add('active');
  state.currentView = name;
}

function showAllCrates() {
  clearDetailBackdrop();
  state.gridMode = 'all';
  updateNavActive('all');
  pushHistory({ view: 'grid', mode: 'all' });
  cleanupSongsUI();
  if (libraryMode === 'crates') {
    renderCratesViewToggle();
    applyCratesViewMode();
  } else {
    document.getElementById('library-filter-row').innerHTML = '';
    renderSongsView();
  }
  showView('grid');
}

// Crate-grid filter modes — single source of truth for the renderer.
// PAIRED EDIT: the same value set is validated server-side in set_config_value()'s
// `crates_view_mode` arm (src-tauri/src/commands.rs); change both together.
const CRATES_VIEW_MODES = ['all', 'released', 'unreleased', 'shelved'];
const CRATES_VIEW_LABELS = { all: 'All', released: 'Released', unreleased: 'Unreleased', shelved: 'Shelved' };
const CRATES_VIEW_OPTIONS = CRATES_VIEW_MODES.map(value => ({ value, label: CRATES_VIEW_LABELS[value] }));

function renderCratesViewToggle() {
  const container = document.getElementById('library-filter-row');
  if (!container) return;
  container.innerHTML = `
    <div class="lib-filter-row">
      ${libModeToggleHtml()}
      <div class="lib-filter-pills">
        ${CRATES_VIEW_OPTIONS.map(o => `
          <button class="lib-filter-pill${o.value === cratesViewMode ? ' active' : ''}"
                  onclick="setCratesViewMode('${o.value}')">${o.label.toUpperCase()}</button>`).join('')}
      </div>
    </div>`;
}

function libModeToggleHtml() {
  return `<div class="view-mode-toggle">
    <button class="view-mode-btn${libraryMode === 'crates' ? ' active' : ''}"
            onclick="switchLibraryMode('crates')">CRATES</button>
    <button class="view-mode-btn${libraryMode === 'songs' ? ' active' : ''}"
            onclick="switchLibraryMode('songs')">BEATS</button>
  </div>`;
}

function cleanupSongsUI() {
  document.getElementById('songs-tag-filter')?.remove();
}

async function switchLibraryMode(mode) {
  if (libraryMode === mode) return;
  libraryMode = mode;
  songsData = null; // invalidate cache on mode switch
  if (mode === 'crates') {
    cleanupSongsUI();
    renderCratesViewToggle();
    applyCratesViewMode();
  } else {
    document.getElementById('library-filter-row').innerHTML = '';
    renderSongsView();
  }
  try {
    await api('/api/config/library-mode', { method: 'POST', body: JSON.stringify({ value: mode }) });
  } catch (e) {}
}

function applyCratesViewMode() {
  if (cratesViewMode === 'all') {
    renderCrateGrid(state.crates);
  } else {
    renderCrateGrid(state.crates.filter(c => (c.status || 'unreleased') === cratesViewMode));
  }
}

async function setCratesViewMode(mode) {
  cratesViewMode = mode;
  renderCratesViewToggle();
  applyCratesViewMode();
  try {
    await api('/api/config/crates-view', { method: 'POST', body: JSON.stringify({ value: mode }) });
  } catch (e) {}
}


// ─── Songs View ──────────────────────────────────────────────────────────────

async function renderSongsView() {
  const grid = document.getElementById('crate-grid');
  grid.innerHTML = `<div style="grid-column:1/-1;padding:40px 0 20px;color:var(--muted);font-family:'IBM Plex Mono',monospace;font-size:12px;">Loading songs…</div>`;

  try {
    if (!songsData) {
      songsData = await api('/api/tracks/all');
    }
    if (!allLibraryTags.length) {
      allLibraryTags = await api('/api/tags/track');
    }
  } catch (e) {
    grid.innerHTML = `<div style="grid-column:1/-1;padding:40px 0;color:var(--muted);font-family:'IBM Plex Mono',monospace;font-size:12px;">Failed to load songs.</div>`;
    return;
  }

  renderSongsSubTabs();
  renderSongsTrackList();
}

function renderSongsSubTabs() {
  const container = document.getElementById('library-filter-row');
  if (!container) return;
  if (songsFilter === 'tags') songsFilter = 'all';
  const hasTagFilters = songsTagFilters.length > 0;
  const tagBtnLabel = hasTagFilters ? `TAGS · ${songsTagFilters.length}` : 'TAGS';
  const tagItems = allLibraryTags.map(tag => {
    const isActive = songsTagFilters.includes(tag);
    // H1: tag names are user-created and can contain apostrophes/quotes. Never
    // interpolate them into an inline JS handler — escHtml doesn't escape `'`, so
    // a tag like `it's` would break out of the handler string. Carry the value in
    // a data-attribute (escHtml is safe for the double-quoted attribute context)
    // and bind the click via the delegated listener at the bottom of this file.
    return `<button class="songs-tag-popover-pill${isActive ? ' active' : ''}"
                data-tag="${escHtml(tag)}">${escHtml(tag)}</button>`;
  }).join('');
  const clearBtn = hasTagFilters
    ? `<button class="songs-tag-popover-clear" onclick="clearSongsTagFilters()">Clear all (${songsTagFilters.length})</button>`
    : '';
  container.innerHTML = `
    <div class="lib-filter-row">
      ${libModeToggleHtml()}
      <div class="lib-filter-pills">
        ${['all', 'favorites'].map(t => `
          <button class="lib-filter-pill${songsFilter === t ? ' active' : ''}"
                  onclick="setSongsFilter('${t}')">${t.toUpperCase()}</button>`).join('')}
        <div class="songs-tag-filter-wrap">
          <button class="lib-filter-pill${hasTagFilters ? ' active' : ''}"
                  id="songs-tag-btn"
                  onclick="toggleSongsTagPopover(event)">${tagBtnLabel}</button>
          <div class="songs-tag-popover" id="songs-tag-popover" style="display:none">
            <div class="songs-tag-popover-pills">${tagItems}</div>
            ${clearBtn}
          </div>
        </div>
      </div>
    </div>`;
}

function setSongsFilter(filter) {
  songsFilter = filter;
  renderSongsSubTabs();
  renderSongsTrackList();
}

function getSongsFilteredTracks() {
  if (!songsData) return [];
  let tracks = songsData;
  if (songsFilter === 'favorites') {
    tracks = tracks.filter(t => t.favorited === 1);
  }
  if (songsTagFilters.length > 0) {
    tracks = tracks.filter(t => songsTagFilters.every(tag => t.tags.includes(tag)));
  }
  if (songsSortKey) {
    tracks = [...tracks].sort((a, b) => {
      const va = songsSortKey === 'title' ? (a.title || '') : (a.crate_name || '');
      const vb = songsSortKey === 'title' ? (b.title || '') : (b.crate_name || '');
      const cmp = va.localeCompare(vb, undefined, { sensitivity: 'base' });
      return songsSortDir === 'asc' ? cmp : -cmp;
    });
  }
  return tracks;
}

function setSongsSort(key) {
  if (songsSortKey === key) {
    songsSortDir = songsSortDir === 'asc' ? 'desc' : 'asc';
  } else {
    songsSortKey = key;
    songsSortDir = 'asc';
  }
  renderSongsTrackList();
}

function renderSongsTrackList() {
  const grid = document.getElementById('crate-grid');

  const tracks = getSongsFilteredTracks();

  if (!tracks.length) {
    grid.innerHTML = `<div style="grid-column:1/-1;padding:40px 0;color:var(--muted);font-family:'IBM Plex Mono',monospace;font-size:12px;">No tracks found.</div>`;
    return;
  }

  const sortArrow = key => {
    if (songsSortKey !== key) return `<span class="songs-sort-icon">⇅</span>`;
    return `<span class="songs-sort-icon active">${songsSortDir === 'asc' ? '↑' : '↓'}</span>`;
  };

  let html = `<div id="songs-track-list" style="grid-column:1/-1;">
    <div class="songs-list-header">
      <div class="songs-th songs-th-num">#</div>
      <div class="songs-th songs-th-art"></div>
      <div class="songs-th songs-th-title songs-th-sortable" onclick="setSongsSort('title')">Title ${sortArrow('title')}</div>
      <div class="songs-th songs-th-crate songs-th-sortable" onclick="setSongsSort('crate')">Crate ${sortArrow('crate')}</div>
      <div class="songs-th songs-th-dur"></div>
      <div class="songs-th songs-th-star"></div>
    </div>`;
  tracks.forEach((t, idx) => {
    const isPlaying = t.id === state.playingTrackId;
    const isActivelyPlaying = isPlaying && state.playing;
    const playIcon = isActivelyPlaying ? '⏸' : '▶';
    html += `
      <div class="track-row${isPlaying ? ' playing' : ''}${isActivelyPlaying ? ' active-playing' : ''}"
           id="songs-row-${t.id}"
           ondblclick="playSongsTrack(${t.id})">
        <div class="track-num songs-track-num">
          <span class="track-num-text">${String(idx + 1).padStart(2, '0')}</span>
          <span class="track-eq-bars" aria-hidden="true"><span class="eq-b"></span><span class="eq-b"></span><span class="eq-b"></span></span>
          <button class="track-play-btn"
                  onclick="event.stopPropagation(); playSongsTrack(${t.id})">${playIcon}</button>
        </div>
        <div class="songs-track-art" id="songs-art-${t.id}">
          <div class="songs-track-art-inner ${crateColor(t.crate_id)}"></div>
        </div>
        <div class="songs-track-title">
          <span class="songs-crate-link" onclick="event.stopPropagation(); openCrateDetail(${t.crate_id})">${escHtml(t.title)}</span>
        </div>
        <div class="songs-track-crate">
          <span class="songs-crate-link" onclick="event.stopPropagation(); openCrateDetail(${t.crate_id})">${escHtml(t.crate_name)}</span>
        </div>
        <div class="songs-track-dur">${t.duration ? formatTime(t.duration) : '—'}</div>
        <button class="songs-track-star${t.favorited ? ' starred' : ''}"
                onclick="toggleFavorite(event, ${t.id}, this)">★</button>
      </div>`;
  });
  html += '</div>';
  grid.innerHTML = html;

  // Lazy-load cover art
  tracks.forEach(t => {
    const el = document.getElementById(`songs-art-${t.id}`);
    if (el) loadCoverArt(el, crateCoverUrl(t.crate_id));
  });

  // Animate EQ bars for currently playing track
  if (state.playingTrackId) refreshSongsPlayState();
}

function refreshSongsPlayState() {
  const list = document.getElementById('songs-track-list');
  if (!list) return;
  list.querySelectorAll('.track-row').forEach(row => {
    const id = parseInt(row.id.replace('songs-row-', ''));
    const isPlaying = id === state.playingTrackId;
    const isActivelyPlaying = isPlaying && state.playing;
    row.classList.toggle('playing', isPlaying);
    row.classList.toggle('active-playing', isActivelyPlaying);
    const playBtn = row.querySelector('.track-play-btn');
    if (playBtn) {
      playBtn.textContent = isActivelyPlaying ? '⏸' : '▶';
    }
  });
}

function playSongsTrack(trackId) {
  if (!songsData) return;
  const visibleTracks = getSongsFilteredTracks();
  const index = visibleTracks.findIndex(t => t.id === trackId);
  if (index === -1) return;
  const track = visibleTracks[index];

  if (state.playingTrackId === trackId) {
    togglePlay();
    return;
  }

  jukeboxActive = false;
  jukeboxUpdatePbIcon();
  hwQueueActive = false;
  state.playingQueue  = visibleTracks.slice();
  state.playingIndex  = index;
  state.playingCrate  = null;
  loadAndPlay(track, null);
  refreshSongsPlayState();
}

function toggleSongsTagPopover(e) {
  e.stopPropagation();
  const pop = document.getElementById('songs-tag-popover');
  if (!pop) return;
  pop.style.display = pop.style.display === 'none' ? '' : 'none';
}

function toggleSongsTagFilter(tag, checked) {
  if (checked && !songsTagFilters.includes(tag)) {
    songsTagFilters.push(tag);
  } else if (!checked) {
    songsTagFilters = songsTagFilters.filter(t => t !== tag);
  }
  renderSongsSubTabs();
  renderSongsTrackList();
  const pop = document.getElementById('songs-tag-popover');
  if (pop) pop.style.display = '';
}

function removeSongsTagFilter(tag) {
  songsTagFilters = songsTagFilters.filter(t => t !== tag);
  renderSongsSubTabs();
  renderSongsTrackList();
}

function clearSongsTagFilters() {
  songsTagFilters = [];
  renderSongsSubTabs();
  renderSongsTrackList();
}

async function showFavorites() {
  // Redirect to Library > Songs > Favorites
  libraryMode = 'songs';
  songsFilter = 'favorites';
  songsData = null;
  clearDetailBackdrop();
  state.gridMode = 'all';
  updateNavActive('all');
  pushHistory({ view: 'grid', mode: 'favorites' });
  document.getElementById('library-filter-row').innerHTML = '';
  await renderSongsView();
  showView('grid');
}

// ─── Home View ────────────────────────────────────────────────────────────────

let scratchpadTimer = null;
let crateScratchpadTimer = null;

async function showHome() {
  clearDetailBackdrop();
  updateNavActive('home');
  showView('home');
  pushHistory({ view: 'home' });
  renderHomeWelcome();
  await renderHomeAurora();
  renderHomeTodos();
}

function buildWelcomeVinyl() {
  const uid = 'welcome-' + Math.random().toString(36).slice(2, 8);
  const cream = (a) => `rgba(237,229,211,${a})`;
  const inkHoney = (a) => `rgba(45,30,8,${a})`;

  const fieldGrooves = [];
  for (let r = 232; r >= 104; r -= 3.6) {
    const a = 0.07 + ((Math.sin(r * 0.31) + 1) * 0.5) * 0.07;
    const w = 0.85 + ((Math.cos(r * 0.41) + 1) * 0.5) * 0.45;
    fieldGrooves.push(`<circle cx="256" cy="256" r="${r.toFixed(1)}" fill="none" stroke="${cream(a.toFixed(3))}" stroke-width="${w.toFixed(2)}"/>`);
  }

  const markers = [
    { r: 234, a: 0.20, w: 1.0 },
    { r: 232, a: 0.16, w: 0.7 },
    { r: 216, a: 0.22, w: 1.4 },
    { r: 176, a: 0.20, w: 1.2 },
    { r: 136, a: 0.22, w: 1.4 },
    { r: 108, a: 0.26, w: 1.6 },
    { r: 104, a: 0.20, w: 1.0 },
  ];
  const markerSvg = markers.map(m =>
    `<circle cx="256" cy="256" r="${m.r}" fill="none" stroke="${cream(m.a)}" stroke-width="${m.w}"/>`
  ).join('');

  return `<svg viewBox="0 0 512 512" xmlns="http://www.w3.org/2000/svg" preserveAspectRatio="xMidYMid meet" style="width:100%;height:100%;display:block;overflow:visible" aria-hidden="true">
    <defs>
      <radialGradient id="disc-${uid}" cx="38%" cy="32%" r="75%">
        <stop offset="0%" stop-color="#1a2419"/>
        <stop offset="55%" stop-color="#0d1510"/>
        <stop offset="100%" stop-color="#060a07"/>
      </radialGradient>
      <radialGradient id="label-${uid}" cx="38%" cy="32%" r="65%">
        <stop offset="0%" stop-color="#f2d89a"/>
        <stop offset="42%" stop-color="#c8a35a"/>
        <stop offset="100%" stop-color="#7e5e20"/>
      </radialGradient>
      <radialGradient id="sheen-${uid}" cx="30%" cy="22%" r="55%">
        <stop offset="0%" stop-color="rgba(200,163,90,0.22)"/>
        <stop offset="100%" stop-color="rgba(200,163,90,0)"/>
      </radialGradient>
      <radialGradient id="counter-${uid}" cx="72%" cy="78%" r="42%">
        <stop offset="0%" stop-color="rgba(237,229,211,0.07)"/>
        <stop offset="100%" stop-color="rgba(237,229,211,0)"/>
      </radialGradient>
    </defs>
    <g>
      <circle cx="256" cy="256" r="238" fill="url(#disc-${uid})"/>
      <circle cx="256" cy="256" r="238" fill="url(#sheen-${uid})"/>
      <circle cx="256" cy="256" r="238" fill="url(#counter-${uid})"/>
      ${fieldGrooves.join('')}
      ${markerSvg}
      <circle cx="256" cy="256" r="237" fill="none" stroke="rgba(237,229,211,0.18)" stroke-width="0.6"/>
      <circle cx="256" cy="256" r="234.5" fill="none" stroke="rgba(0,0,0,0.50)" stroke-width="1.4"/>
      <circle cx="256" cy="256" r="86" fill="url(#label-${uid})"/>
      <circle cx="256" cy="256" r="82" fill="none" stroke="rgba(45,30,8,0.48)" stroke-width="1"/>
      <circle cx="256" cy="256" r="78" fill="none" stroke="rgba(45,30,8,0.20)" stroke-width="0.5"/>
      <circle cx="256" cy="256" r="86" fill="none" stroke="rgba(255,243,210,0.22)" stroke-width="0.7" stroke-dasharray="70 270" transform="rotate(-100 256 256)"/>
      <circle cx="256" cy="256" r="86" fill="none" stroke="rgba(0,0,0,0.35)" stroke-width="0.9" stroke-dasharray="80 260" transform="rotate(75 256 256)"/>
      <path id="arc-${uid}" d="M 186 256 a 70 70 0 1 1 140 0 a 70 70 0 1 1 -140 0" fill="none"/>
      <text font-family="Archivo, sans-serif" font-weight="600" font-size="9" letter-spacing="2.2" fill="${inkHoney(0.62)}">
        <textPath href="#arc-${uid}" startOffset="0">BEATCRATE  ·  MIDNIGHT WAX  ·  SIDE A  ·  MW-001  ·  </textPath>
      </text>
      <g transform="translate(256 256)">
        <text y="-30" text-anchor="middle" font-family="Archivo, sans-serif" font-weight="500" font-size="7" letter-spacing="2" fill="${inkHoney(0.5)}">33⅓ RPM</text>
        <text y="42" text-anchor="middle" font-family="Archivo, sans-serif" font-weight="600" font-size="9" letter-spacing="-0.2" fill="${inkHoney(0.7)}">BC</text>
      </g>
      <circle cx="256" cy="256" r="21" fill="rgba(45,30,8,0.65)"/>
      <circle cx="256" cy="256" r="18" fill="#060a07"/>
      <circle cx="256" cy="256" r="14" fill="#020403"/>
      <ellipse cx="250.5" cy="250.5" rx="3.5" ry="1.2" fill="rgba(237,229,211,0.32)" transform="rotate(-35 250.5 250.5)"/>
      <ellipse cx="262" cy="262" rx="2" ry="0.7" fill="rgba(237,229,211,0.10)" transform="rotate(-35 262 262)"/>
    </g>
  </svg>`;
}

function splitGreetingWords(text) {
  return text.split(/(\s+)/).map(token => {
    if (/^\s+$/.test(token)) return token;
    return `<span class="word-mask"><span class="word-inner">${token}</span></span>`;
  }).join('');
}

function renderHomeWelcome() {
  if (sessionStorage.getItem('welcomed')) {
    document.body.classList.remove('welcoming', 'mesh-fullscreen');
    return;
  }
  sessionStorage.setItem('welcomed', '1');

  const h = new Date().getHours();
  const greeting = h < 4  ? 'Burning the midnight oil'
                 : h < 7  ? 'Up before the birds'
                 : h < 12 ? "Crate o'clock"
                 : h < 17 ? 'Afternoon flow state'
                 : h < 21 ? 'Golden hour'
                 :          'Night shift';

  const overlay = document.createElement('div');
  overlay.className = 'welcome-overlay';
  overlay.innerHTML = `
    <div class="welcome-stage">
      <div class="welcome-vinyl" aria-hidden="true">
        <div class="welcome-vinyl-spin">${buildWelcomeVinyl()}</div>
      </div>
      <div class="welcome-greet">${splitGreetingWords(greeting + '.')}</div>
    </div>
  `;
  document.body.appendChild(overlay);

  const vinyl = overlay.querySelector('.welcome-vinyl');
  const greet = overlay.querySelector('.welcome-greet');

  // Per-word stagger, sublinear so long strings don't drag past the budget.
  const inners = greet.querySelectorAll('.word-inner');
  const n = inners.length;
  const stagger = Math.min(120, 480 / Math.max(n, 1));
  inners.forEach((el, i) => el.style.setProperty('--d', (i * stagger).toFixed(0) + 'ms'));

  // Phase timings (ms from mount):
  //   450  vinyl ghosts in (1.1s ease) — rotation already running on inner wrapper
  //   900  greeting words start revealing (0.95s each, sublinear stagger, ≤1.4s total)
  //  2900  overlay begins fading out (0.9s ease)
  //  3800  greeting fully gone → chrome + home content fade in (0.8s ease)
  //  4700  content fully in → mesh begins slow zoom-out to its layout slot (1.6s ease)
  requestAnimationFrame(() => {
    setTimeout(() => vinyl.classList.add('in'),  450);
    setTimeout(() => greet.classList.add('playing'),  900);
    setTimeout(() => overlay.classList.add('out'), 2900);
    setTimeout(() => {
      overlay.remove();
      document.body.classList.remove('welcoming');
    }, 3800);
    setTimeout(() => document.body.classList.remove('mesh-fullscreen'), 4700);
  });
}

let hwTopTracks   = [];   // active play queue (set by hwPlayTrack)
let hwWeekTracks  = [];   // This Week top tracks
let hwAllTimeTracks = []; // All Time top tracks
let hwQueueActive = false;
let _hwActiveTab  = 'week';
let hwCareerTimelineChart = null;
let hwCareerDowChart      = null;
let hwCareerMonthChart    = null;


// ─── Analytics Card (Home) ───────────────────────────────────────────────────

function getWeekRange() {
  const now = new Date();
  const day = now.getDay();
  const mon = new Date(now);
  mon.setDate(now.getDate() - ((day + 6) % 7));
  const sun = new Date(mon);
  sun.setDate(mon.getDate() + 6);
  const mo = ['JAN','FEB','MAR','APR','MAY','JUN','JUL','AUG','SEP','OCT','NOV','DEC'];
  const fmt = d => `${mo[d.getMonth()]} ${String(d.getDate()).padStart(2, '0')}`;
  return `${fmt(mon)} – ${fmt(sun)}`;
}

function hwChartColors() {
  return {
    accent:  '#d4561a',
    fill:    'rgba(212,86,26,0.15)',
    grid:    'rgba(240,232,216,0.07)',
    tick:    '#7a6e5c',
    tooltip: { bg: '#2a2520', border: 'rgba(240,232,216,0.09)', text: '#e8ddc8' },
  };
}

function hwBuildTrackRows(tracks, prefix) {
  return [1, 2, 3].map(rank => {
    const t = tracks[rank - 1];
    if (!t) return `
    <div class="hw-track-row">
      <span class="hw-track-rank">${rank}</span>
      <div class="hw-track-art"><div class="pb-art-inner cg"><div class="pb-art-ring"></div></div></div>
      <div class="hw-track-info">
        <div class="hw-track-name">—</div>
        <div class="hw-track-crate">—</div>
      </div>
      <span class="hw-track-count">—</span>
    </div>`;
    return `
    <div class="hw-track-row">
      <span class="hw-track-rank">${rank}</span>
      <div class="hw-track-art">
        <div class="pb-art-inner cg" id="hw-art-${prefix}-${t.track_id}"><div class="pb-art-ring"></div></div>
        <button class="hw-play-btn" id="hw-play-${prefix}-${t.track_id}" onclick="event.stopPropagation(); hwPlayTrack(${t.track_id},'${prefix}')"><span class="hw-play-icon">▶</span></button>
        <span class="hw-eq-bars"><span class="eq-b"></span><span class="eq-b"></span><span class="eq-b"></span></span>
      </div>
      <div class="hw-track-info">
        <div class="hw-track-name"><span style="cursor:pointer" onclick="openCrateDetail(${t.crate_id})">${escHtml(t.title)}</span></div>
        <div class="hw-track-crate"><span style="cursor:pointer" onclick="openCrateDetail(${t.crate_id})">${escHtml(t.crate_name)}</span></div>
      </div>
      <span class="hw-track-count">${t.plays}</span>
    </div>`;
  }).join('');
}

function hwLoadCoverArt(tracks, prefix) {
  tracks.forEach(t => {
    const artEl = document.getElementById(`hw-art-${prefix}-${t.track_id}`);
    if (artEl) { const cu = crateCoverUrl(t.crate_id); loadCoverArt(artEl, cu, `<img src="${cu}" alt="" style="width:100%;height:100%;object-fit:cover;">`); }
  });
}

// ─── Aurora Home Rendering ────────────────────────────────────────────────────

let homeActiveMode = 'week';
let homeWeekData   = null;
let homeAllTimeData = null;

const HOME_VINYL_SVG = `<svg width="20" height="20" viewBox="0 0 100 100" fill="none" aria-hidden="true">
  <circle cx="50" cy="50" r="48" fill="#0a0e0b"/>
  <circle cx="50" cy="50" r="38" fill="none" stroke="#ede5d3" stroke-opacity="0.3" stroke-width="1.2"/>
  <circle cx="50" cy="50" r="28" fill="none" stroke="#ede5d3" stroke-opacity="0.3" stroke-width="1.2"/>
  <circle cx="50" cy="50" r="18" fill="#c8a35a"/>
  <circle cx="50" cy="50" r="4" fill="#0a0e0b"/>
</svg>`;

let _homeClockInterval = null;

function updateHomeClock() {
  const now = new Date();
  const time = document.getElementById('home-hdr-time');
  if (time) {
    const h24 = now.getHours();
    const m = String(now.getMinutes()).padStart(2, '0');
    const ampm = h24 >= 12 ? 'PM' : 'AM';
    const h12 = h24 % 12 || 12;
    time.textContent = `${h12}:${m} ${ampm}`;
  }
  const date = document.getElementById('home-hdr-date');
  if (date) {
    const days = ['SUN','MON','TUE','WED','THU','FRI','SAT'];
    const mons = ['JAN','FEB','MAR','APR','MAY','JUN','JUL','AUG','SEP','OCT','NOV','DEC'];
    date.textContent = `${days[now.getDay()]} · ${mons[now.getMonth()]} ${now.getDate()}`;
  }
}

function startHomeClockTick() {
  updateHomeClock();
  if (_homeClockInterval) return;
  _homeClockInterval = setInterval(updateHomeClock, 30 * 1000);
}

async function renderHomeAurora() {
  startHomeClockTick();

  homeActiveMode = 'week';
  homeAllTimeData = null; // reset cache on each home load

  let data = { totalPlays: 0, tracksPlayed: 0, notesAdded: 0, tagsAdded: 0, todosDone: 0, topTracks: [] };
  try { data = await api('/api/stats/weekly'); } catch (_) {}
  homeWeekData = data;
  hwWeekTracks = data.topTracks || [];

  const syncEl = document.getElementById('synced-label');
  if (syncEl && data.lastSyncTime) {
    const d = new Date(data.lastSyncTime);
    const month = d.toLocaleString('en-US', { month: 'short' });
    const day   = d.getDate();
    const hour  = d.getHours() % 12 || 12;
    const min   = d.getMinutes().toString().padStart(2, '0');
    const ampm  = d.getHours() < 12 ? 'AM' : 'PM';
    syncEl.textContent = `SYNCED · ${month} ${day} · ${hour}:${min} ${ampm}`;
  }

  renderHomeHero(data, 'week');
  renderHomeMostPlayed(hwWeekTracks, 'week');
}

function renderHomeHero(data, mode) {
  const hero = document.getElementById('home-hero');
  if (!hero) return;
  homeActiveMode = mode;

  const plays  = data.totalPlays   ?? 0;
  const tracks = data.tracksPlayed ?? 0;
  const notes  = mode === 'week' ? (data.notesAdded ?? 0) : (data.notesCount ?? 0);
  const tags   = mode === 'week' ? (data.tagsAdded  ?? 0) : (data.tagsCount  ?? 0);
  const todos  = data.todosDone ?? 0;

  const controlRow = document.getElementById('home-control-row');
  if (controlRow) {
    controlRow.innerHTML = `
      <div class="lib-filter-row">
        <div class="view-mode-toggle">
          <button class="view-mode-btn${mode === 'week' ? ' active' : ''}" onclick="homeToggle('week')">THIS WEEK</button>
          <button class="view-mode-btn${mode === 'alltime' ? ' active' : ''}" onclick="homeToggle('alltime')">ALL TIME</button>
        </div>
      </div>`;
  }

  hero.innerHTML = `
    <div class="home-hero-col">
      <div class="home-hero-display" data-odo="plays"></div>
      <div class="home-hero-sub">${(() => {
        if (mode !== 'week') return 'spins';
        const prev = data.prevWeekPlays ?? 0;
        if (prev === 0 && plays === 0) return 'spins';
        if (prev === 0) return `spins · <span class="home-hero-delta-up">↑ new</span>`;
        const pct = Math.round(((plays - prev) / prev) * 100);
        const arrow = pct >= 0 ? '↑' : '↓';
        const cls = pct >= 0 ? 'home-hero-delta-up' : 'home-hero-delta-down';
        return `spins · <span class="${cls}">${arrow} ${Math.abs(pct)}% vs last</span>`;
      })()}</div>
    </div>
    ${[
      [tracks, 'tracks played', 'tracks'],
      [notes,  'notes added',   'notes'],
      [tags,   'tags created',  'tags'],
      [todos,  'to-dos done',   'todos'],
    ].map(([_n, l, key]) => `
      <div class="home-hero-col">
        <div class="home-hero-stat" data-odo="${key}"></div>
        <div class="home-hero-caption">${l}</div>
      </div>`).join('')}`;

  mountOdometer(hero.querySelector('[data-odo="plays"]'),  plays,  'plays');
  mountOdometer(hero.querySelector('[data-odo="tracks"]'), tracks, 'tracks');
  mountOdometer(hero.querySelector('[data-odo="notes"]'),  notes,  'notes');
  mountOdometer(hero.querySelector('[data-odo="tags"]'),   tags,   'tags');
  mountOdometer(hero.querySelector('[data-odo="todos"]'),  todos,  'todos');
}

const _odoCache = {};

function mountOdometer(el, value, key) {
  if (!el) return;
  const str = String(value);
  const prevStr = (key && _odoCache[key] != null) ? _odoCache[key] : '';

  if (key) _odoCache[key] = str;

  if (!/\d/.test(str)) {
    el.classList.remove('odo');
    el.textContent = str;
    return;
  }

  // Value unchanged → render statically, no animation
  if (prevStr === str) {
    el.classList.add('odo');
    el.innerHTML = str.split('').map(ch =>
      /\d/.test(ch)
        ? `<span class="odo-digit"><span class="odo-strip" style="transform:translateY(-${ch}em)">${
            '0123456789'.split('').map(d => `<span>${d}</span>`).join('')
          }</span></span>`
        : `<span class="odo-static">${ch}</span>`
    ).join('');
    return;
  }

  el.classList.add('odo');

  const padLen = Math.max(str.length, prevStr.length);
  const padded     = str.padStart(padLen, ' ');
  const paddedPrev = prevStr.padStart(padLen, ' ');

  el.innerHTML = padded.split('').map((ch, i) => {
    if (/\d/.test(ch)) {
      const prevCh = paddedPrev[i];
      const start  = /\d/.test(prevCh) ? parseInt(prevCh, 10) : 0;
      const final  = parseInt(ch, 10);
      const digits = '0123456789'.split('').map(d => `<span>${d}</span>`).join('');
      return `<span class="odo-digit"><span class="odo-strip" data-start="${start}" data-final="${final}">${digits}</span></span>`;
    }
    if (ch === ' ') return '';
    return `<span class="odo-static">${ch}</span>`;
  }).join('');

  const strips = el.querySelectorAll('.odo-strip');
  strips.forEach(s => {
    const start = parseInt(s.dataset.start, 10);
    s.style.transition = 'none';
    s.style.transform = `translateY(-${start}em)`;
  });

  // force reflow so the initial transform sticks before the transition fires
  void el.offsetWidth;

  requestAnimationFrame(() => {
    strips.forEach((s, i) => {
      const final = parseInt(s.dataset.final, 10);
      const delay = i * 90;
      s.style.transition = `transform 1100ms cubic-bezier(0.22, 0.61, 0.36, 1) ${delay}ms`;
      s.style.transform = `translateY(-${final}em)`;
    });
  });
}

async function homeToggle(mode) {
  if (homeActiveMode === mode) return;

  if (mode === 'alltime' && !homeAllTimeData) {
    try {
      homeAllTimeData = await api('/api/stats/alltime');
      hwAllTimeTracks = homeAllTimeData.topTracks || [];
    } catch (_) {
      homeAllTimeData = { totalPlays: 0, notesCount: 0, tagsCount: 0, topTracks: [] };
    }
  }

  const data   = mode === 'week' ? homeWeekData : homeAllTimeData;
  const tracks = mode === 'week' ? hwWeekTracks : hwAllTimeTracks;
  renderHomeHero(data, mode);
  renderHomeMostPlayed(tracks, mode);

  const period = document.getElementById('home-mp-period');
  if (period) period.textContent = mode === 'week' ? 'THIS WEEK' : 'ALL TIME';
}

function renderHomeMostPlayed(tracks, prefix) {
  const list = document.getElementById('home-tracks-list');
  if (!list) return;
  if (!tracks || !tracks.length) {
    list.innerHTML = '<div class="home-tracks-empty">No plays this period</div>';
    return;
  }
  list.innerHTML = tracks.map((t, i) => `
    <div class="home-track-row" data-track-id="${t.track_id}">
      <div class="home-track-num" onclick="hwPlayTrack(${t.track_id}, '${prefix}')">
        <span class="home-track-num-text">${String(i + 1).padStart(2, '0')}</span>
        <button class="home-track-play-btn">▶</button>
        <div class="home-track-eq-bars"><span class="eq-b"></span><span class="eq-b"></span><span class="eq-b"></span></div>
      </div>
      <div class="home-track-art">
        <img src="${crateCoverUrl(t.crate_id)}" alt="" onerror="this.style.display='none'">
      </div>
      <div class="home-track-info">
        <div class="home-track-title"><span class="home-track-link" onclick="openCrateDetail(${t.crate_id})">${escHtml(t.title)}</span></div>
        <div class="home-track-artist"><span class="home-track-link" onclick="openCrateDetail(${t.crate_id})">${escHtml(t.crate_name)}</span></div>
      </div>
      <div class="home-track-plays">×${t.plays}</div>
    </div>`).join('');
}

function hwViewAll() { showAllCrates(); }


// ─── Career Arc View ─────────────────────────────────────────────────────────

function showCareerArc() {
  clearDetailBackdrop();
  updateNavActive('career');
  showView('career');
  pushHistory({ view: 'career' });
  renderCareerView();
}

async function renderCareerView() {
  const main = document.getElementById('career-main');
  if (!main) return;
  main.innerHTML = '<div class="career-loading">Loading…</div>';

  let summary, timeline, dowRows, plugins;
  try {
    [summary, timeline, dowRows, plugins] = await Promise.all([
      api('/api/insights/summary'),
      api('/api/insights/timeline'),
      api('/api/insights/day-of-week'),
      api('/api/insights/plugins'),
    ]);
  } catch (_) {
    main.innerHTML = '<div class="career-loading">Failed to load Career Arc data.</div>';
    return;
  }

  const MON_SHORT = ['Jan','Feb','Mar','Apr','May','Jun','Jul','Aug','Sep','Oct','Nov','Dec'];
  const DOW_SHORT = ['Sun','Mon','Tue','Wed','Thu','Fri','Sat'];

  const byYear = {};
  timeline.forEach(r => { const yr = r.month.slice(0, 4); byYear[yr] = (byYear[yr] || 0) + r.cnt; });
  const peakYear = Object.entries(byYear).sort((a, b) => b[1] - a[1])[0]?.[0] ?? '—';

  const byMonthIdx = new Array(12).fill(0);
  timeline.forEach(r => { byMonthIdx[parseInt(r.month.slice(5, 7), 10) - 1] += r.cnt; });
  const favMonthIdx = byMonthIdx.indexOf(Math.max(...byMonthIdx));
  const favMonth = byMonthIdx.some(v => v > 0) ? MON_SHORT[favMonthIdx] : '—';

  let favDay = '—';
  if (dowRows.length) {
    const maxRow = dowRows.reduce((a, b) => b.cnt > a.cnt ? b : a, dowRows[0]);
    favDay = DOW_SHORT[parseInt(maxRow.dow, 10)] ?? '—';
  }

  const totalProjects = summary.totals?.total ?? 0;
  const avgBpm = summary.totals?.avg_bpm ?? '—';

  const timelineData   = timeline.map(r => r.cnt);
  const step = Math.max(1, Math.ceil(timeline.length / 10));
  const timelineLabels = timeline.filter((_, i) => i % step === 0).map(r => r.month.slice(0, 7));

  const DOW_DISPLAY = [1,2,3,4,5,6,0];
  const DOW_LABELS  = ['Mon','Tue','Wed','Thu','Fri','Sat','Sun'];
  const dowMap = {};
  dowRows.forEach(r => { dowMap[r.dow] = r.cnt; });
  const dowData = DOW_DISPLAY.map((d, i) => [DOW_LABELS[i], dowMap[String(d)] || 0]);

  const monthCounts = new Array(12).fill(0);
  timeline.forEach(r => { monthCounts[parseInt(r.month.slice(5, 7), 10) - 1] += r.cnt; });
  const monthData = MON_SHORT.map((m, i) => [m, monthCounts[i]]);

  const top8 = (plugins.plugins || []).slice(0, 8);
  const pluginMax = top8[0]?.cnt || 1;

  main.innerHTML = `
    <div class="career-header-band">
      <h1 class="career-hdr-title">Career arc</h1>
    </div>

    <div class="career-stats">
      ${[
        [totalProjects, 'PROJECTS'],
        [avgBpm,        'AVG BPM'],
        [peakYear,      'PEAK YEAR'],
        [favMonth,      'FAV MONTH'],
        [favDay,        'FAV DAY'],
      ].map(([n, l], i) => `
        <div class="career-stat-col${i > 0 ? ' bordered' : ''}">
          <div class="career-stat-n">${n}</div>
          <div class="career-stat-l">${l}</div>
        </div>`).join('')}
    </div>
    <div class="career-panel">
      <div class="career-sect-hdr">
        <span class="career-sect-title">Beats over time</span>
        <span class="career-sect-meta">MONTHLY · ${timeline.length} months</span>
      </div>
      ${buildLineChartHtml(timelineData, timelineLabels)}
    </div>

    <div class="career-two-col">
      <div class="career-panel">
        <div class="career-sect-hdr"><span class="career-sect-title">Day of week</span></div>
        ${buildBarChartHtml(dowData)}
      </div>
      <div class="career-panel">
        <div class="career-sect-hdr"><span class="career-sect-title">Month of year</span></div>
        ${buildBarChartHtml(monthData)}
      </div>
    </div>

    <div class="career-panel">
      <div class="career-sect-hdr" style="margin-bottom:14px">
        <span class="career-sect-title">Top plugins</span>
        <span class="career-sect-meta">BY PROJECTS USED IN</span>
      </div>
      ${top8.length
        ? top8.map(p => `
          <div class="career-plugin-row">
            <div class="career-plugin-name">${escHtml(p.plugin_name)}</div>
            <div class="career-plugin-bar-wrap">
              <div class="career-plugin-bar" style="width:${Math.round(p.cnt / pluginMax * 100)}%"></div>
            </div>
            <div class="career-plugin-cnt">${p.cnt}</div>
          </div>`).join('')
        : '<div class="career-loading">No plugin data found.</div>'}
    </div>`;
}

function buildLineChartHtml(data, labels) {
  if (!data || !data.length) return '<div class="career-loading">No data available.</div>';
  const W = 1000, H = 200;
  const max = Math.max(...data, 1);

  const pts = data.map((v, i) => [
    (i / Math.max(data.length - 1, 1)) * W,
    H - (v / max) * (H - 20) - 10,
  ]);

  const out = [`M ${pts[0][0].toFixed(2)} ${pts[0][1].toFixed(2)}`];
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[Math.max(i - 1, 0)];
    const p1 = pts[i];
    const p2 = pts[i + 1];
    const p3 = pts[Math.min(i + 2, pts.length - 1)];
    const cp1x = p1[0] + (p2[0] - p0[0]) / 6;
    const cp1y = p1[1] + (p2[1] - p0[1]) / 6;
    const cp2x = p2[0] - (p3[0] - p1[0]) / 6;
    const cp2y = p2[1] - (p3[1] - p1[1]) / 6;
    out.push(`C ${cp1x.toFixed(2)} ${cp1y.toFixed(2)},${cp2x.toFixed(2)} ${cp2y.toFixed(2)},${p2[0].toFixed(2)} ${p2[1].toFixed(2)}`);
  }
  const linePath = out.join(' ');
  const fillPath = linePath + ` L ${W} ${H} L 0 ${H} Z`;
  const gid = 'lcg' + Math.floor(Math.random() * 99999);

  // SVG chart occupies y=10..190 inside H=200; CSS height=180px → 9px top/bottom inset
  return `<div class="career-line-chart">
    <div class="career-chart-row">
      <div class="career-yaxis" style="height:180px;padding:9px 0">
        <span>${max}</span><span>${Math.round(max / 2)}</span><span>0</span>
      </div>
      <div style="flex:1;min-width:0">
        <svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" style="width:100%;height:180px;display:block;overflow:visible">
          <defs>
            <linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1">
              <stop offset="0%" stop-color="#c8a35a" stop-opacity="0.35"/>
              <stop offset="100%" stop-color="#c8a35a" stop-opacity="0"/>
            </linearGradient>
          </defs>
          <path d="${fillPath}" fill="url(#${gid})"/>
          <path d="${linePath}" fill="none" stroke="#c8a35a" stroke-width="1.5" vector-effect="non-scaling-stroke" stroke-linecap="round" stroke-linejoin="round"/>
        </svg>
        ${labels.length ? `<div class="career-chart-xlabels">${labels.map(l => `<span>${l}</span>`).join('')}</div>` : ''}
      </div>
    </div>
  </div>`;
}

function buildBarChartHtml(data) {
  if (!data || !data.length) return '';
  const max = Math.max(...data.map(d => d[1]), 1);
  return `<div class="career-bar-chart">
    <div class="career-chart-row">
      <div class="career-yaxis" style="height:130px">
        <span>${max}</span><span>${Math.round(max / 2)}</span><span>0</span>
      </div>
      <div style="flex:1;min-width:0">
        <div class="career-bar-bars">
          ${data.map(([, v]) => `<div class="career-bar-col"><div class="career-bar-inner" style="height:${Math.round((v / max) * 100)}%"></div></div>`).join('')}
        </div>
        <div class="career-bar-labels">
          ${data.map(([l]) => `<span>${l}</span>`).join('')}
        </div>
      </div>
    </div>
  </div>`;
}

// Re-fetches weekly stats and re-renders the hero if the home view is visible.
async function refreshStats() {
  const hero = document.getElementById('home-hero');
  if (!hero || !hero.offsetParent || homeActiveMode !== 'week') return;
  try {
    const data = await api('/api/stats/weekly');
    homeWeekData = data;
    hwWeekTracks = data.topTracks || [];
    renderHomeHero(data, 'week');
  } catch (_) {}
}


function hwPlayTrack(trackId, prefix) {
  if (state.playingTrackId === trackId) { togglePlay(); return; }
  const tracks = (prefix === 'at' || prefix === 'alltime') ? hwAllTimeTracks : hwWeekTracks;
  const t = tracks.find(x => x.track_id === trackId);
  if (!t) return;
  jukeboxActive = false;
  jukeboxUpdatePbIcon();
  hwQueueActive = true;
  hwTopTracks = tracks;
  state.playingQueue = tracks.map(x => ({ id: x.track_id, title: x.title, crate_id: x.crate_id, crate_name: x.crate_name }));
  state.playingIndex = tracks.indexOf(t);
  state.playingCrate = null;
  loadAndPlay(
    { id: t.track_id, title: t.title, crate_id: t.crate_id, crate_name: t.crate_name },
    { id: t.crate_id, name: t.crate_name }
  );
}

function refreshHwPlayState() {
  const allTracks = [...hwWeekTracks, ...hwAllTimeTracks];
  const seen = new Set();
  allTracks.forEach(t => {
    if (seen.has(t.track_id)) return;
    seen.add(t.track_id);
    const row = document.querySelector(`.home-track-row[data-track-id="${t.track_id}"]`);
    if (!row) return;
    const isActive = t.track_id === state.playingTrackId;
    row.classList.toggle('playing', isActive);
    row.classList.toggle('active-playing', isActive && state.playing);
    const btn = row.querySelector('.home-track-play-btn');
    if (btn) btn.textContent = (isActive && state.playing) ? '⏸' : '▶';
  });
}

// ─── Home collapsible state ───────────────────────────────────────────────────

let todoCollapsed       = false;
let scratchpadCollapsed = false;

async function toggleTodo() {
  todoCollapsed = !todoCollapsed;
  const todoWrap = document.getElementById('todo-wrap');
  if (todoWrap) todoWrap.style.display = todoCollapsed ? 'none' : '';
  document.getElementById('todo-label-btn')?.classList.toggle('collapsed', todoCollapsed);
  try {
    await api('/api/config/todo-collapsed', {
      method: 'POST',
      body: JSON.stringify({ value: todoCollapsed ? '1' : '0' }),
    });
  } catch (e) {}
}

async function toggleScratchpad() {
  scratchpadCollapsed = !scratchpadCollapsed;
  const spWrap = document.getElementById('scratchpad-wrap');
  if (spWrap) spWrap.style.display = scratchpadCollapsed ? 'none' : '';
  document.getElementById('scratchpad-label-btn')?.classList.toggle('collapsed', scratchpadCollapsed);
  try {
    await api('/api/config/scratchpad-collapsed', {
      method: 'POST',
      body: JSON.stringify({ value: scratchpadCollapsed ? '1' : '0' }),
    });
  } catch (e) {}
}

// ─── Jukebox (transport mode) ─────────────────────────────────────────────────

let jukeboxActive  = false; // true when the jukebox is the current playback source
let jukeboxTrack   = null;
let jukeboxCrate   = null;
let jukeboxHistory = []; // stack of previously played jukebox { track, crate } entries

async function jukeboxLoad() {
  try {
    const track = await api('/api/tracks/random');
    if (!track) return;
    // /api/tracks/random doesn't include tags — fetch them from the crate tracks endpoint
    try {
      const crateTracks = await api(`/api/crates/${track.crate_id}/tracks`);
      const match = crateTracks.find(t => t.id === track.id);
      if (match) track.tags = match.tags;
    } catch (_) {}
    const crate = state.crates.find(c => c.id === track.crate_id) || { id: track.crate_id, name: track.crate_name };
    jukeboxTrack = track;
    jukeboxCrate = crate;
    jukeboxRefreshDisplay();
  } catch (e) {}
}

async function jukeboxSkip() {
  // Save current track to history before loading a new one
  if (jukeboxTrack) jukeboxHistory.push({ track: jukeboxTrack, crate: jukeboxCrate });
  await jukeboxLoad();
  if (!jukeboxTrack) return;
  // If jukebox was playing, auto-play the new track; otherwise just display it
  if (jukeboxActive) {
    hwQueueActive = false;
    state.playingQueue  = [jukeboxTrack];
    state.playingIndex  = 0;
    state.playingCrate  = jukeboxCrate;
    loadAndPlay(jukeboxTrack, jukeboxCrate); // .then calls jukeboxSyncWidget
  }
}

async function jukeboxDice() {
  // Always load a new random track and auto-play it, regardless of current state
  if (jukeboxTrack) jukeboxHistory.push({ track: jukeboxTrack, crate: jukeboxCrate });
  await jukeboxLoad();
  if (!jukeboxTrack) return;
  jukeboxActive       = true;
  hwQueueActive = false;
  state.playingQueue  = [jukeboxTrack];
  state.playingIndex  = 0;
  state.playingCrate  = jukeboxCrate;
  loadAndPlay(jukeboxTrack, jukeboxCrate);
}

function jukeboxPrev() {
  if (jukeboxHistory.length === 0) return;
  const prev = jukeboxHistory.pop();
  jukeboxTrack = prev.track;
  jukeboxCrate = prev.crate;
  jukeboxRefreshDisplay();
  if (jukeboxActive) {
    hwQueueActive = false;
    state.playingQueue  = [jukeboxTrack];
    state.playingIndex  = 0;
    state.playingCrate  = jukeboxCrate;
    loadAndPlay(jukeboxTrack, jukeboxCrate);
  }
}

function jukeboxSyncWidget() {
  jukeboxUpdatePbIcon();
}

function jukeboxUpdatePbIcon() {
  const btn = document.getElementById('btn-jukebox');
  if (!btn) return;
  btn.classList.toggle('active', jukeboxActive);
  btn.title = jukeboxActive ? 'Beat Roulette active' : 'Beat Roulette';
}

async function toggleJukeboxMode() {
  // If nothing is loaded, treat the dice press as "play a random track now"
  if (!state.playingTrackId) {
    await jukeboxDice();
    return;
  }

  const wasActive = jukeboxActive;
  jukeboxActive = !jukeboxActive;

  if (wasActive && state.playingTrackId && state.playingCrate?.id) {
    // Roulette was just turned off while a track is playing — rebuild the
    // playback queue from the current track's crate so that sequential
    // playback resumes naturally when the song ends.
    const crateId = state.playingCrate.id;
    try {
      const crateTracks = await api(`/api/crates/${crateId}/tracks`);
      const idx = crateTracks.findIndex(t => t.id === state.playingTrackId);
      if (idx !== -1) {
        hwQueueActive = false;
        state.playingQueue = crateTracks;
        state.playingIndex = idx;
      }
    } catch (_) {}
  }

  jukeboxSyncWidget();
}

function openTodoInput() {
  const btn = document.getElementById('home-todo-add-btn');
  const input = document.getElementById('home-todo-input');
  if (btn) btn.style.display = 'none';
  if (input) { input.style.display = ''; input.focus(); }
}

// ─── Home To-Do List ──────────────────────────────────────────────────────────

async function renderHomeTodos() {
  const list = document.getElementById('home-todo-list');
  const input = document.getElementById('home-todo-input');
  if (!list || !input) return;
  const todoWrap = document.getElementById('todo-wrap');
  if (todoWrap) todoWrap.style.display = todoCollapsed ? 'none' : '';
  document.getElementById('todo-label-btn')?.classList.toggle('collapsed', todoCollapsed);

  try {
    const todos = await api('/api/todos');
    drawTodos(todos, list);
  } catch (e) {
    list.innerHTML = '';
  }

  // Remove any existing listener by cloning
  const fresh = input.cloneNode(true);
  input.parentNode.replaceChild(fresh, input);
  fresh.addEventListener('keydown', async e => {
    if (e.key !== 'Enter') return;
    const text = fresh.value.trim();
    if (!text) return;
    fresh.value = '';
    fresh.style.display = 'none';
    const addBtn = document.getElementById('home-todo-add-btn');
    if (addBtn) addBtn.style.display = '';
    try {
      const todo = await api('/api/todos', {
        method: 'POST',
        body: JSON.stringify({ text }),
      });
      const current = document.getElementById('home-todo-list');
      if (!current) return;
      // Remove empty state if present
      const empty = current.querySelector('.home-todo-empty');
      if (empty) empty.remove();
      current.prepend(makeTodoEl(todo));
      updateTodoOpenCount();
      refreshStats();
    } catch (e) {}
  });
  fresh.addEventListener('blur', () => {
    if (!fresh.value.trim()) {
      fresh.style.display = 'none';
      const btn = document.getElementById('home-todo-add-btn');
      if (btn) btn.style.display = '';
    }
  });
}

function updateTodoOpenCount() {
  const list = document.getElementById('home-todo-list');
  const label = document.getElementById('home-plate-open');
  if (!label) return;
  const count = list ? list.querySelectorAll('.home-todo-item').length : 0;
  label.textContent = count > 0 ? `${count} OPEN` : '';
}

function drawTodos(todos, container) {
  if (!todos.length) {
    renderTodoEmptyState(container);
    updateTodoOpenCount();
    return;
  }
  container.innerHTML = '';
  todos.forEach(todo => container.appendChild(makeTodoEl(todo)));
  updateTodoOpenCount();
}

function renderTodoEmptyState(container) {
  if (!container) return;
  container.innerHTML = `
    <div class="home-todo-empty">
      <div class="home-todo-empty-mark">·</div>
      <div class="home-todo-empty-text">Plate's clear, get cooking…</div>
    </div>`;
}

function makeTodoEl(todo) {
  const item = document.createElement('div');
  item.className = 'home-todo-item';
  item.dataset.id = todo.id;
  item.dataset.sortOrder = todo.sort_order ?? 0;
  item.draggable = true;

  // Drag handle
  const handle = document.createElement('span');
  handle.className = 'home-todo-handle';
  handle.textContent = '⠿';
  handle.title = 'Drag to reorder';

  const cb = document.createElement('input');
  cb.type = 'checkbox';
  cb.className = 'home-todo-check';
  cb.checked = false;

  const label = document.createElement('span');
  label.className = 'home-todo-text';
  label.textContent = todo.text;

  // Delete button
  const del = document.createElement('button');
  del.className = 'home-todo-delete';
  del.textContent = '×';
  del.title = 'Delete';

  item.appendChild(handle);
  item.appendChild(cb);
  item.appendChild(label);
  item.appendChild(del);

  // Complete (checkbox)
  cb.addEventListener('change', () => {
    item.classList.add('completing');
    setTimeout(async () => {
      try {
        await api(`/api/todos/${todo.id}`, {
          method: 'PATCH',
          body: JSON.stringify({ completed: 1 }),
        });
        item.remove();
        const list = document.getElementById('home-todo-list');
        if (list && !list.querySelector('.home-todo-item')) {
          renderTodoEmptyState(list);
        }
        updateTodoOpenCount();
        refreshStats();
      } catch (e) {
        item.classList.remove('completing');
        cb.checked = false;
      }
    }, 900);
  });

  // Inline edit — textarea so long todos wrap the same way they display.
  label.addEventListener('click', () => {
    const input = document.createElement('textarea');
    input.className = 'home-todo-edit';
    input.value = label.textContent;
    input.rows = 1;
    item.draggable = false;
    item.replaceChild(input, label);

    const autosize = () => {
      input.style.height = 'auto';
      input.style.height = input.scrollHeight + 'px';
    };

    input.focus();
    input.select();
    autosize();
    input.addEventListener('input', autosize);

    async function saveEdit() {
      const newText = input.value.trim();
      if (newText && newText !== todo.text) {
        try {
          await api(`/api/todos/${todo.id}/text`, {
            method: 'PATCH',
            body: JSON.stringify({ text: newText }),
          });
          todo.text = newText;
        } catch (e) { /* revert below */ }
      }
      label.textContent = todo.text;
      item.draggable = true;
      item.replaceChild(label, input);
    }

    input.addEventListener('blur', saveEdit);
    input.addEventListener('keydown', e => {
      // Enter saves; Shift+Enter inserts a newline (rare for todos but available).
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); input.blur(); }
      if (e.key === 'Escape') { input.removeEventListener('blur', saveEdit); item.draggable = true; item.replaceChild(label, input); }
    });
  });

  // Delete
  del.addEventListener('click', e => {
    e.stopPropagation();
    api(`/api/todos/${todo.id}`, { method: 'DELETE' }).then(() => {
      item.remove();
      const list = document.getElementById('home-todo-list');
      if (list && !list.querySelector('.home-todo-item')) {
        renderTodoEmptyState(list);
      }
      updateTodoOpenCount();
      refreshStats();
    }).catch(() => {});
  });

  // Drag-to-reorder
  item.addEventListener('dragstart', e => {
    todoDragSrc = item;
    item.classList.add('todo-dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', todo.id);
  });
  item.addEventListener('dragend', () => {
    item.classList.remove('todo-dragging');
    clearTodoDragIndicators();
    todoDragSrc = null;
    todoDragOver = null;
    todoDragPos = null;
  });
  item.addEventListener('dragover', e => {
    e.preventDefault();
    if (!todoDragSrc || todoDragSrc === item) return;
    const rect = item.getBoundingClientRect();
    const pos = e.clientY < rect.top + rect.height / 2 ? 'top' : 'bottom';
    if (todoDragOver !== item || todoDragPos !== pos) {
      clearTodoDragIndicators();
      todoDragOver = item;
      todoDragPos = pos;
      item.classList.add(pos === 'top' ? 'todo-drag-over-top' : 'todo-drag-over-bottom');
    }
    e.dataTransfer.dropEffect = 'move';
  });
  item.addEventListener('drop', e => {
    e.preventDefault();
    if (!todoDragSrc || todoDragSrc === item) { clearTodoDragIndicators(); return; }
    const list = document.getElementById('home-todo-list');
    if (!list) return;
    clearTodoDragIndicators();
    // Reorder in DOM
    const pos = todoDragPos;
    if (pos === 'top') {
      list.insertBefore(todoDragSrc, item);
    } else {
      list.insertBefore(todoDragSrc, item.nextSibling);
    }
    // Persist new order
    saveTodoOrder(list);
  });

  return item;
}

function clearTodoDragIndicators() {
  const list = document.getElementById('home-todo-list');
  if (!list) return;
  list.querySelectorAll('.home-todo-item').forEach(el => {
    el.classList.remove('todo-drag-over-top', 'todo-drag-over-bottom');
  });
}

async function saveTodoOrder(listEl) {
  const items = Array.from(listEl.querySelectorAll('.home-todo-item'));
  const payload = items.map((el, i) => ({ id: Number(el.dataset.id), sort_order: i + 1 }));
  items.forEach((el, i) => { el.dataset.sortOrder = i + 1; });
  try {
    await api('/api/todos/order', { method: 'PUT', body: JSON.stringify(payload) });
  } catch (e) { console.error('Failed to save todo order:', e); }
}

async function loadScratchpad() {
  const spWrap = document.getElementById('scratchpad-wrap');
  if (spWrap) spWrap.style.display = scratchpadCollapsed ? 'none' : '';
  document.getElementById('scratchpad-label-btn')?.classList.toggle('collapsed', scratchpadCollapsed);
  try {
    const data = await api('/api/scratchpad');
    const ta = document.getElementById('home-scratchpad');
    if (ta) {
      ta.value = data.content || '';
      setupScratchpad(ta);
    }
  } catch (e) {}
}

function autoResizeScratchpad(el) {
  el.style.height = 'auto';
  el.style.height = Math.min(el.scrollHeight, 220) + 'px';
}

function setupScratchpad(ta) {
  // Remove any existing listeners by cloning the node
  const fresh = ta.cloneNode(true);
  ta.parentNode.replaceChild(fresh, ta);

  autoResizeScratchpad(fresh);
  fresh.addEventListener('input', () => {
    autoResizeScratchpad(fresh);
    clearTimeout(scratchpadTimer);
    scratchpadTimer = setTimeout(() => saveScratchpad(fresh.value), 600);
  });
  fresh.addEventListener('blur', () => {
    clearTimeout(scratchpadTimer);
    saveScratchpad(fresh.value);
  });
}

async function saveScratchpad(content) {
  try {
    await api('/api/scratchpad', {
      method: 'PUT',
      body: JSON.stringify({ content }),
    });
  } catch (e) {}
}

function setupCrateScratchpad(ta, crateId) {
  const fresh = ta.cloneNode(true);
  ta.parentNode.replaceChild(fresh, ta);
  fresh.addEventListener('input', () => {
    clearTimeout(crateScratchpadTimer);
    crateScratchpadTimer = setTimeout(() => saveCrateScratchpad(crateId, fresh.value), 600);
  });
  fresh.addEventListener('blur', () => {
    clearTimeout(crateScratchpadTimer);
    saveCrateScratchpad(crateId, fresh.value);
  });
}

async function saveCrateScratchpad(crateId, content) {
  try {
    await api(`/api/crates/${crateId}/scratchpad`, {
      method: 'PUT',
      body: JSON.stringify({ content }),
    });
  } catch (e) {}
}

function updateNavActive(nav) {
  ['home', 'all', 'career'].forEach(n => {
    document.getElementById(`tnav-${n}`)?.classList.toggle('active', n === nav);
  });
}


let cratesViewMode   = 'all';  // 'all' | 'released' | 'unreleased' | 'shelved'
let libraryMode      = 'crates'; // 'crates' | 'songs'
let songsData        = null;     // cached flat track list
let songsFilter      = 'all';    // 'all' | 'favorites'
let songsTagFilters  = [];       // active tag filter chips
let allLibraryTags   = [];       // autocomplete source
let songsSortKey     = null;     // null | 'title' | 'crate'
let songsSortDir     = 'asc';    // 'asc' | 'desc'

// ─── Crate Grid ──────────────────────────────────────────────────────────────

const CRATE_COLORS = ['ca','cb','cc','cd','ce','cf','cg','ch'];

// Fallback wash colors keyed to crate palette classes (lighter stop of each gradient)
const CRATE_WASH_RGB = {
  ca: [106, 56, 24],
  cb: [28,  80, 40],
  cc: [42,  24, 72],
  cd: [80,  56, 24],
  ce: [24,  48, 80],
  cf: [80,  24, 24],
  cg: [48,  48, 24],
  ch: [24,  64, 64],
};

function crateColor(crateId) {
  return CRATE_COLORS[crateId % CRATE_COLORS.length];
}

function crateInitial(name) {
  return (name || '?')[0].toUpperCase();
}

const cratePlayOverlayHtml = id =>
  `<button class="crate-play-hint" onclick="event.stopPropagation(); playCrateInPlace(${id})"><span class="crate-play-icon">▶</span></button><span class="crate-eq-bars"><span class="eq-b"></span><span class="eq-b"></span><span class="eq-b"></span></span>`;

const crateStatusOverlayHtml = c =>
  `<span class="crate-status-chip ${c.status || 'unreleased'}">${STATUS_LABELS[c.status || 'unreleased']}</span>`;

function renderCrateGrid(crates) {
  const grid = document.getElementById('crate-grid');
  if (!crates.length) {
    grid.innerHTML = `
      <div class="empty-state" style="grid-column:1/-1;padding:60px 0;">
        <div class="empty-state-title">No crates found</div>
        <div class="empty-state-sub">Add some beat folders to your albums directory</div>
      </div>`;
    return;
  }

  grid.innerHTML = crates.map(c => {
    const color = crateColor(c.id);
    const artInner = `
      <div class="crate-art-inner ${color}">
        <div class="crate-label">
          <div class="crate-label-ring"></div>
          <div class="crate-label-title">${escHtml(c.name)}</div>
        </div>
      </div>
      ${cratePlayOverlayHtml(c.id)}${crateStatusOverlayHtml(c)}`;
    return `
      <div class="crate-card" data-crate-id="${c.id}">
        <div class="crate-art" id="crate-art-${c.id}" onclick="openCrateDetail(${c.id})">
          ${artInner}
        </div>
        <div class="crate-name" onclick="openCrateDetail(${c.id})">${escHtml(c.name)}</div>
        <div class="crate-meta">${c.track_count} tracks</div>
      </div>`;
  }).join('');

  // Lazy-load cover images
  crates.forEach(c => {
    const artEl = document.getElementById(`crate-art-${c.id}`);
    if (!artEl) return;
    const url = crateCoverUrl(c.id);
    loadCoverArt(artEl, url,
      `<img class="crate-art-img" src="${url}" alt="${escHtml(c.name)}">${cratePlayOverlayHtml(c.id)}${crateStatusOverlayHtml(c)}`);
  });

  refreshCrateGridPlayState();
}


async function playCrateInPlace(crateId) {
  if (state.playingCrate?.id === crateId && state.playingTrackId) {
    togglePlay();
    return;
  }
  const crate = state.crates.find(c => c.id === crateId);
  if (!crate) return;
  const tracks = await api(`/api/crates/${crateId}/tracks`);
  if (!tracks || !tracks.length) return;
  jukeboxActive = false;
  jukeboxUpdatePbIcon();
  hwQueueActive = false;
  state.playingQueue  = tracks.slice();
  state.playingIndex  = 0;
  state.playingCrate  = crate;
  loadAndPlay(tracks[0], crate);
}

function refreshCrateGridPlayState() {
  document.querySelectorAll('#crate-grid .crate-card[data-crate-id]').forEach(card => {
    const crateId = parseInt(card.dataset.crateId);
    const isThisCrate    = state.playingCrate?.id === crateId && !!state.playingTrackId;
    const isActivePlaying = isThisCrate && state.playing;
    card.classList.toggle('playing', isThisCrate);
    card.classList.toggle('active-playing', isActivePlaying);
    const icon = card.querySelector('.crate-play-icon');
    if (icon) icon.textContent = isActivePlaying ? '⏸' : '▶';
  });
}

// ─── Crate Detail ─────────────────────────────────────────────────────────────

// Verify the cover loads, then resolve its URL for the backdrop (or a fallback
// color). Returns a Promise so it can run in parallel with the tracks API call.
// NOTE: we used to draw the cover into a canvas and toDataURL() it, but under
// Tauri the cover is an asset: URL (a different origin than the page), which
// taints the canvas — toDataURL() then throws SecurityError. The dataURL was
// only ever used as a CSS background-image, so we just use the URL directly.
function loadBackdropContent(coverSrc, fallbackRgb) {
  return new Promise(resolve => {
    if (!coverSrc) { resolve(fallbackRgb ? { type: 'color', rgb: fallbackRgb } : { type: 'none' }); return; }
    const thumb = new Image();
    thumb.onload  = () => resolve({ type: 'image', url: coverSrc });
    thumb.onerror = () => resolve(fallbackRgb ? { type: 'color', rgb: fallbackRgb } : { type: 'none' });
    thumb.src = coverSrc;
  });
}

function commitBackdrop(content) {
  const backdrop = document.getElementById('detail-backdrop');
  const img      = document.getElementById('detail-backdrop-img');
  if (content.type === 'image') {
    img.style.backgroundImage = `url(${content.url})`;
    img.style.backgroundColor = '';
  } else if (content.type === 'color') {
    img.style.backgroundImage = '';
    img.style.backgroundColor = `rgb(${content.rgb[0]},${content.rgb[1]},${content.rgb[2]})`;
  }
  backdrop.classList.add('active');
}

function applyDetailBackdrop(content) {
  const backdrop = document.getElementById('detail-backdrop');
  if (backdrop.classList.contains('active')) {
    // Crossfade: fade out, swap, fade in
    backdrop.classList.remove('active');
    setTimeout(() => commitBackdrop(content), 200);
  } else {
    commitBackdrop(content);
  }
}

function clearDetailBackdrop() {
  document.getElementById('detail-backdrop').classList.remove('active');
}

function navigateToPlayingCrate() {
  if (state.playingCrate) {
    openCrateDetail(state.playingCrate.id);
    return;
  }
  // Fallback: playing from a flat list (e.g. Favorites) — look up crate via queue
  if (state.playingTrackId) {
    const track = state.playingQueue.find(t => t.id === state.playingTrackId);
    if (track && track.crate_id) {
      const crate = state.crates.find(c => c.id === track.crate_id);
      if (crate) openCrateDetail(crate.id);
    }
  }
}

async function openCrateDetail(crateId) {
  const crate = state.crates.find(c => c.id === crateId);
  if (!crate) return;

  state.activeCrate = crate;
  state.selectedTrackId = null;

  // Kick off backdrop pre-render, tracks, and scratchpad in parallel
  const coverSrc = crateCoverUrl(crate.id);
  const backdropPromise    = loadBackdropContent(coverSrc, CRATE_WASH_RGB[crateColor(crate.id)]);
  const tracksPromise      = api(`/api/crates/${crateId}/tracks`);
  const scratchpadPromise  = api(`/api/crates/${crateId}/scratchpad`);
  const [backdropContent, tracks, scratchpadData] = await Promise.all([backdropPromise, tracksPromise, scratchpadPromise]);

  state.tracks = tracks;
  document.getElementById('inspector-content').innerHTML = '<div class="inspector-empty">Select a track</div>';

  updateNavActive(null);

  // Art: show gradient placeholder, then swap in cover if available
  const artContainer = document.getElementById('detail-art-container');
  const color = crateColor(crate.id);
  artContainer.className = '';
  artContainer.innerHTML = `
    <div class="detail-art-inner ${color}">
      <div class="detail-art-label">
        <div class="detail-art-ring">
          <span class="detail-art-ring-text">${crateInitial(crate.name)}</span>
        </div>
      </div>
    </div>`;
  if (backdropContent.type === 'image') {
    // Cover loaded successfully — swap art (non-blocking, already rendered for backdrop)
    artContainer.innerHTML = `<img src="${coverSrc}" style="width:100%;height:100%;object-fit:cover;display:block;" alt="${escHtml(crate.name)}">`;
  }

  document.getElementById('detail-name').textContent = crate.name;
  renderProducerField(crate);
  document.getElementById('detail-sub').textContent = `${tracks.length} tracks`;
  renderCrateStatus(crate);
  renderTrackList(tracks, crate);

  // Crate scratch pad
  const ta = document.getElementById('crate-scratchpad');
  if (ta) {
    ta.value = scratchpadData?.content || '';
    setupCrateScratchpad(ta, crateId);
  }

  // Backdrop is pre-rendered — apply before revealing the view
  applyDetailBackdrop(backdropContent);
  pushHistory({ view: 'detail', crateId });
  showView('detail');
}

// ─── Producer Inline Edit ─────────────────────────────────────────────────────

function renderProducerField(crate) {
  const container = document.getElementById('detail-producer');

  function showDisplay() {
    container.innerHTML = '';
    const el = document.createElement('div');
    const val = crate.producer || '';
    el.className = 'detail-producer-display' + (val ? '' : ' is-empty');
    el.textContent = val || 'Add producer…';
    el.addEventListener('click', showInput);
    container.appendChild(el);
  }

  function showInput() {
    container.innerHTML = '';
    const input = document.createElement('input');
    input.className = 'detail-producer-input';
    input.value = crate.producer || '';
    input.placeholder = 'Add producer…';
    input.maxLength = 100;
    input.autocomplete = 'off';
    container.appendChild(input);
    input.focus();
    input.select();

    let committed = false;

    async function commit() {
      if (committed) return;
      committed = true;
      const val = input.value.trim();
      crate.producer = val || null;
      const c = state.crates.find(c => c.id === crate.id);
      if (c) c.producer = crate.producer;
      await saveProducer(crate.id, crate.producer);
      showDisplay();
    }

    input.addEventListener('blur', commit);
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter') { e.preventDefault(); input.blur(); }
      if (e.key === 'Escape') { committed = true; showDisplay(); }
    });
  }

  showDisplay();
}

async function saveProducer(crateId, producer) {
  try {
    await api(`/api/crates/${crateId}/producer`, {
      method: 'PATCH',
      body: JSON.stringify({ producer }),
    });
  } catch (err) {
    console.error('Failed to save producer:', err);
  }
}

// ─── Crate Status ─────────────────────────────────────────────────────────────

const STATUS_LABELS = { unreleased: 'Unreleased', released: 'Released', shelved: 'Shelved' };
const STATUS_CYCLE  = { unreleased: 'released', released: 'shelved', shelved: 'unreleased' };

function renderCrateStatus(crate) {
  const container = document.getElementById('detail-status');
  if (!container) return;
  const status = crate.status || 'unreleased';
  const chip = document.createElement('span');
  chip.className = `crate-status-chip ${status}`;
  chip.textContent = STATUS_LABELS[status];
  chip.title = 'Click to change status';
  chip.addEventListener('click', () => setCrateStatus(crate, STATUS_CYCLE[status]));
  container.innerHTML = '';
  container.appendChild(chip);
}

async function setCrateStatus(crate, newStatus) {
  try {
    await api(`/api/crates/${crate.id}/status`, {
      method: 'PATCH',
      body: JSON.stringify({ status: newStatus }),
    });
    crate.status = newStatus;
    const c = state.crates.find(c => c.id === crate.id);
    if (c) c.status = newStatus;
    renderCrateStatus(crate);
  } catch (err) {
    console.error('Failed to update status:', err);
  }
}

// ─── Track Inspector ──────────────────────────────────────────────────────────

function toggleInspector() {
  state.inspectorOpen = !state.inspectorOpen;
  document.getElementById('inspector-panel').classList.toggle('open', state.inspectorOpen);
}

function closeInspector() {
  if (!state.inspectorOpen) return;
  state.selectedTrackId = null;
  document.querySelectorAll('#tracks-list .track-row').forEach(r => r.classList.remove('selected'));
  state.inspectorOpen = false;
  document.getElementById('inspector-panel').classList.remove('open');
}

function renderInspector(track) {
  const container = document.getElementById('inspector-content');
  container.innerHTML = `
    <div class="inspector-title">${escHtml(track.title)}</div>
    <div class="inspector-section">
      <div class="inspector-label">Notes</div>
      <ul class="inspector-notes-list" id="inspector-notes-list"></ul>
      <div class="inspector-note-add" id="inspector-note-add">
        <button class="inspector-note-add-btn" id="inspector-note-add-btn">+ add note</button>
        <input class="inspector-note-input" id="inspector-note-input" placeholder="Type note…" style="display:none">
      </div>
    </div>`;

  // Notes — collapsible add input
  const addBtn = document.getElementById('inspector-note-add-btn');
  const noteInput = document.getElementById('inspector-note-input');
  addBtn.addEventListener('click', () => {
    addBtn.style.display = 'none';
    noteInput.style.display = '';
    noteInput.focus();
  });
  noteInput.addEventListener('keydown', e => {
    if (e.key === 'Escape') {
      e.stopPropagation();
      noteInput.style.display = 'none';
      addBtn.style.display = '';
      noteInput.value = '';
    }
  });
  noteInput.addEventListener('keyup', async e => {
    if (e.key !== 'Enter') return;
    const note = noteInput.value.trim();
    if (!note) return;
    noteInput.value = '';
    noteInput.style.display = 'none';
    addBtn.style.display = '';
    await addTrackNote(track.id, note);
  });

  renderNotesList(track, document.getElementById('inspector-notes-list'));
}

async function addTrackTag(trackId, tag) {
  try {
    await api(`/api/tracks/${trackId}/tags`, {
      method: 'POST',
      body: JSON.stringify({ tag }),
    });
    const track = state.tracks.find(t => t.id === trackId);
    if (track) {
      if (!track.tags) track.tags = [];
      if (!track.tags.includes(tag)) track.tags.push(tag);
    }
    refreshStats();
  } catch (err) {
    console.error('Failed to add track tag:', err);
  }
}

async function removeTrackTag(trackId, tag) {
  try {
    await api(`/api/tracks/${trackId}/tags/${encodeURIComponent(tag)}`, {
      method: 'DELETE',
    });
    const track = state.tracks.find(t => t.id === trackId);
    if (track) {
      track.tags = (track.tags || []).filter(t => t !== tag);
      if (state.selectedTrackId === trackId) renderInspector(track);
    }
    refreshStats();
  } catch (err) {
    console.error('Failed to remove track tag:', err);
  }
}

// ─── Inline tag helpers (crate detail tracklist) ─────────────────────────────

function buildTrackTagsInnerHTML(track) {
  // Render ALL pills as full chips. applyTagOverflow() runs after DOM insert
  // and measures actual width; if pills overflow, the trailing ones get hidden
  // and a +M badge with hover-popover is added. This way the column only
  // collapses when it actually needs to — no false +M when there's room.
  const pillHtml = (tag) =>
    `<span class="track-tag tag-ink"><span class="track-tag-label">${escHtml(tag)}</span><button class="track-tag-remove" data-tag="${escHtml(tag)}" onclick="event.stopPropagation(); removeTrackTagFromRow(${track.id}, this.dataset.tag)" title="Remove tag">×</button></span>`;
  const chips = (track.tags || []).map(pillHtml).join('');
  return `<div class="track-tags-chips">${chips}</div><button class="track-tag-add-btn" onclick="event.stopPropagation(); openTagPickerDropdown(${track.id}, this)" title="Add tag">+</button>`;
}

// Measure chips overflow within a single .track-tags element; if pills don't
// fit, hide trailing ones and insert a +M badge whose popover holds the
// hidden tag pills (cloned, so their × handlers still target the right tag).
function applyTagOverflow(tagsEl) {
  if (!tagsEl) return;
  const chipsEl = tagsEl.querySelector('.track-tags-chips');
  if (!chipsEl) return;

  // Reset: remove any existing +M and unhide pills, so we measure clean state.
  const existing = tagsEl.querySelector('.track-tag-overflow');
  if (existing) existing.remove();
  const pills = Array.from(chipsEl.children);
  pills.forEach(p => { p.style.display = ''; });

  // No overflow without +M? Done.
  if (chipsEl.scrollWidth <= chipsEl.clientWidth + 1) return;

  // Add the +M badge first (it takes width from chips, which shrinks chips and
  // may force more pills to hide). Insert before the + add button.
  const addBtn = tagsEl.querySelector('.track-tag-add-btn');
  const overflowEl = document.createElement('span');
  overflowEl.className = 'track-tag tag-ink track-tag-overflow';
  overflowEl.addEventListener('click', e => e.stopPropagation());
  const overflowLabel = document.createElement('span');
  overflowLabel.className = 'track-tag-label';
  overflowLabel.textContent = '+0';
  const popover = document.createElement('span');
  popover.className = 'track-tag-overflow-popover';
  overflowEl.appendChild(overflowLabel);
  overflowEl.appendChild(popover);
  tagsEl.insertBefore(overflowEl, addBtn);

  // Hide pills from the end until they fit.
  let hiddenCount = 0;
  for (let i = pills.length - 1; i >= 0; i--) {
    if (chipsEl.scrollWidth <= chipsEl.clientWidth + 1) break;
    pills[i].style.display = 'none';
    hiddenCount++;
  }

  if (hiddenCount === 0) {
    // +M's own width was enough to push chips into overflow even though
    // every pill technically fit — fall back to removing the +M.
    overflowEl.remove();
    return;
  }

  overflowLabel.textContent = '+' + hiddenCount;
  for (let i = pills.length - hiddenCount; i < pills.length; i++) {
    const clone = pills[i].cloneNode(true);
    clone.style.display = '';
    popover.appendChild(clone);
  }
}

function applyTagOverflowToAllRows() {
  document.querySelectorAll('.track-row .track-tags').forEach(applyTagOverflow);
}

// Resize the window → tag column width changes → re-balance pills + +M.
// Debounced via rAF so we don't thrash measurements during a drag.
let _tagOverflowResizeRaf = 0;
window.addEventListener('resize', () => {
  if (_tagOverflowResizeRaf) return;
  _tagOverflowResizeRaf = requestAnimationFrame(() => {
    _tagOverflowResizeRaf = 0;
    applyTagOverflowToAllRows();
    const pbTags = document.getElementById('pb-tags');
    if (pbTags) applyPbTagOverflow(pbTags);
  });
});

async function removeTrackTagFromRow(trackId, tag) {
  await removeTrackTag(trackId, tag);
  refreshTrackRowTagsCell(trackId);
}

function refreshTrackRowTagsCell(trackId) {
  const track = state.tracks.find(t => t.id === trackId);
  if (!track) return;
  const row = document.getElementById(`track-row-${trackId}`);
  if (!row) return;
  const tagsDiv = row.querySelector('.track-tags');
  if (tagsDiv) {
    tagsDiv.innerHTML = buildTrackTagsInnerHTML(track);
    applyTagOverflow(tagsDiv);
  }
}

async function openTagPickerDropdown(trackId, btn) {
  closeTagPickerDropdown();

  const track = state.tracks.find(t => t.id === trackId);
  const existingTags = new Set((track && track.tags) || []);

  let allTags = [];
  try { allTags = await api('/api/tags/track'); } catch (_) {}

  const panel = document.createElement('div');
  panel.className = 'tag-picker-dropdown';
  panel.id = 'tag-picker-panel';

  const inputRow = document.createElement('div');
  inputRow.className = 'tag-picker-input-row';
  const input = document.createElement('input');
  input.className = 'tag-picker-input';
  input.placeholder = 'filter or type new tag…';
  input.maxLength = 40;
  inputRow.appendChild(input);

  const chipsDiv = document.createElement('div');
  chipsDiv.className = 'tag-picker-chips';

  panel.appendChild(inputRow);
  panel.appendChild(chipsDiv);
  document.body.appendChild(panel);

  function positionPanel() {
    const rect = btn.getBoundingClientRect();
    const panelH = 260;
    const spaceBelow = window.innerHeight - rect.bottom;
    panel.style.left = Math.min(rect.left, window.innerWidth - 234) + 'px';
    if (spaceBelow >= panelH || spaceBelow >= rect.top) {
      panel.style.top = (rect.bottom + 4) + 'px';
      panel.style.bottom = '';
    } else {
      panel.style.top = '';
      panel.style.bottom = (window.innerHeight - rect.top + 4) + 'px';
    }
  }
  positionPanel();

  function renderChips(filter) {
    const f = filter.toLowerCase();
    const visible = allTags.filter(t => !f || t.includes(f));
    chipsDiv.innerHTML = '';
    visible.forEach(tag => {
      const chip = document.createElement('button');
      chip.className = 'tag-picker-chip' + (existingTags.has(tag) ? ' already-has' : '');
      chip.textContent = tag;
      chip.addEventListener('mousedown', async e => {
        e.preventDefault();
        e.stopPropagation();
        if (!existingTags.has(tag)) await addTrackTag(trackId, tag);
        dismiss();
        refreshTrackRowTagsCell(trackId);
      });
      chipsDiv.appendChild(chip);
    });
  }
  renderChips('');

  input.addEventListener('input', () => renderChips(input.value.trim()));

  let dismissed = false;
  let docClickTimer = null;
  function dismiss() {
    if (dismissed) return;
    dismissed = true;
    // Cancel the deferred attach if we're closing before it fired (Esc/Enter),
    // otherwise the listener would attach after dismiss and leak.
    if (docClickTimer) { clearTimeout(docClickTimer); docClickTimer = null; }
    panel.remove();
    document.removeEventListener('click', onDocClick, true);
  }
  panel._dismiss = dismiss;

  input.addEventListener('keydown', async e => {
    e.stopPropagation();
    if (e.key === 'Enter') {
      const raw = e.target.value.trim().toLowerCase();
      if (raw && !existingTags.has(raw)) await addTrackTag(trackId, raw);
      dismiss();
      if (raw) refreshTrackRowTagsCell(trackId);
    } else if (e.key === 'Escape') {
      dismiss();
    }
  });

  function onDocClick(e) {
    if (!panel.contains(e.target) && e.target !== btn) dismiss();
  }
  docClickTimer = setTimeout(() => {
    docClickTimer = null;
    document.addEventListener('click', onDocClick, true);
  }, 0);

  input.focus();
}

function closeTagPickerDropdown() {
  const existing = document.getElementById('tag-picker-panel');
  if (existing) { if (existing._dismiss) existing._dismiss(); else existing.remove(); }
}

async function addTrackNote(trackId, note) {
  try {
    const result = await api(`/api/tracks/${trackId}/notes`, {
      method: 'POST',
      body: JSON.stringify({ note }),
    });
    const newNote = { id: result.id, note, created_at: Math.floor(Date.now() / 1000), sort_order: result.sort_order, completed: 0 };

    const stateTrack = state.tracks.find(t => t.id === trackId);
    if (stateTrack) {
      if (!stateTrack.notes) stateTrack.notes = [];
      stateTrack.notes.push(newNote);
      if (state.selectedTrackId === trackId) {
        renderNotesList(stateTrack, document.getElementById('inspector-notes-list'));
      }
    }

    // Mirror into the playback-bar popover cache if it's tracking this same track
    if (state.pbNoteTrack && state.pbNoteTrack.id === trackId) {
      if (!state.pbNoteTrack.notes) state.pbNoteTrack.notes = [];
      // Avoid double-append if the popover was the originator and the same array was mutated above
      if (state.pbNoteTrack !== stateTrack) state.pbNoteTrack.notes.push(newNote);
      if (state.pbNotePopoverOpen) {
        renderNotesList(state.pbNoteTrack, document.getElementById('pb-note-list'));
      }
      updatePbNoteBadge();
    }
    refreshStats();
  } catch (err) {
    console.error('Failed to add note:', err);
  }
}

async function deleteTrackNote(trackId, noteId) {
  try {
    await api(`/api/tracks/${trackId}/notes/${noteId}`, { method: 'DELETE' });

    const stateTrack = state.tracks.find(t => t.id === trackId);
    if (stateTrack) {
      stateTrack.notes = (stateTrack.notes || []).filter(n => n.id !== noteId);
      if (state.selectedTrackId === trackId) {
        renderNotesList(stateTrack, document.getElementById('inspector-notes-list'));
      }
    }

    if (state.pbNoteTrack && state.pbNoteTrack.id === trackId) {
      if (state.pbNoteTrack !== stateTrack) {
        state.pbNoteTrack.notes = (state.pbNoteTrack.notes || []).filter(n => n.id !== noteId);
      }
      if (state.pbNotePopoverOpen) {
        renderNotesList(state.pbNoteTrack, document.getElementById('pb-note-list'));
      }
      updatePbNoteBadge();
    }
    refreshStats();
  } catch (err) {
    console.error('Failed to delete note:', err);
  }
}

function renderNotesList(track, listEl) {
  if (!listEl) return;
  listEl.innerHTML = '';
  (track.notes || []).forEach(note => listEl.appendChild(makeNoteEl(note, track, listEl)));
}

function makeNoteEl(note, track, listEl) {
  const li = document.createElement('li');
  li.className = 'inspector-note-item' + (note.completed ? ' completed' : '');
  li.dataset.id = note.id;
  li.dataset.sortOrder = note.sort_order ?? 0;
  li.draggable = true;

  const handle = document.createElement('span');
  handle.className = 'inspector-note-handle';
  handle.textContent = '⠿';
  handle.title = 'Drag to reorder';

  const check = document.createElement('input');
  check.type = 'checkbox';
  check.className = 'inspector-note-check';
  check.checked = !!note.completed;
  check.addEventListener('change', e => {
    e.stopPropagation();
    const newCompleted = check.checked ? 1 : 0;
    note.completed = newCompleted;
    li.classList.toggle('completed', !!newCompleted);
    if (state.playingTrackId === track.id) updatePbNoteBadge();
    api(`/api/tracks/${track.id}/notes/${note.id}`, {
      method: 'PATCH',
      body: JSON.stringify({ completed: newCompleted }),
    }).catch(() => {
      // Revert optimistic update on failure
      note.completed = newCompleted ? 0 : 1;
      check.checked = !check.checked;
      li.classList.toggle('completed', !!note.completed);
      if (state.playingTrackId === track.id) updatePbNoteBadge();
    });
  });

  const text = document.createElement('span');
  text.className = 'inspector-note-text';
  text.textContent = note.note;

  const del = document.createElement('button');
  del.className = 'inspector-note-del';
  del.textContent = '×';
  del.title = 'Remove';
  del.addEventListener('click', e => {
    e.stopPropagation();
    deleteTrackNote(track.id, note.id);
  });

  text.addEventListener('click', () => {
    const input = document.createElement('input');
    input.type = 'text';
    input.className = 'inspector-note-edit';
    input.value = text.textContent;
    li.draggable = false;
    li.replaceChild(input, text);
    input.focus();
    input.select();

    async function saveEdit() {
      const newNote = input.value.trim();
      if (newNote && newNote !== note.note) {
        try {
          await api(`/api/tracks/${track.id}/notes/${note.id}`, {
            method: 'PATCH',
            body: JSON.stringify({ note: newNote }),
          });
          note.note = newNote;
          const n = (track.notes || []).find(n => n.id === note.id);
          if (n) n.note = newNote;
        } catch (e) { /* revert below */ }
      }
      text.textContent = note.note;
      li.draggable = true;
      li.replaceChild(text, input);
    }

    input.addEventListener('blur', saveEdit);
    input.addEventListener('keydown', e => {
      if (e.key === 'Enter') { e.preventDefault(); input.blur(); }
      if (e.key === 'Escape') { input.removeEventListener('blur', saveEdit); li.draggable = true; li.replaceChild(text, input); }
    });
  });

  // Drag-to-reorder
  li.addEventListener('dragstart', e => {
    noteDragSrc = li;
    li.classList.add('note-dragging');
    e.dataTransfer.effectAllowed = 'move';
    e.dataTransfer.setData('text/plain', note.id);
  });
  li.addEventListener('dragend', () => {
    li.classList.remove('note-dragging');
    clearNoteDragIndicators(listEl);
    noteDragSrc = null;
    noteDragOver = null;
    noteDragPos = null;
  });
  li.addEventListener('dragover', e => {
    e.preventDefault();
    if (!noteDragSrc || noteDragSrc === li) return;
    const rect = li.getBoundingClientRect();
    const pos = e.clientY < rect.top + rect.height / 2 ? 'top' : 'bottom';
    if (noteDragOver !== li || noteDragPos !== pos) {
      clearNoteDragIndicators(listEl);
      noteDragOver = li;
      noteDragPos = pos;
      li.classList.add(pos === 'top' ? 'note-drag-over-top' : 'note-drag-over-bottom');
    }
    e.dataTransfer.dropEffect = 'move';
  });
  li.addEventListener('drop', e => {
    e.preventDefault();
    if (!noteDragSrc || noteDragSrc === li) { clearNoteDragIndicators(listEl); return; }
    if (!listEl) return;
    clearNoteDragIndicators(listEl);
    if (noteDragPos === 'top') {
      listEl.insertBefore(noteDragSrc, li);
    } else {
      listEl.insertBefore(noteDragSrc, li.nextSibling);
    }
    saveNoteOrder(listEl, track.id);
  });

  li.append(handle, check, text, del);
  return li;
}

function clearNoteDragIndicators(listEl) {
  if (!listEl) return;
  listEl.querySelectorAll('.inspector-note-item').forEach(el => {
    el.classList.remove('note-drag-over-top', 'note-drag-over-bottom');
  });
}

async function saveNoteOrder(listEl, trackId) {
  const items = Array.from(listEl.querySelectorAll('.inspector-note-item'));
  const payload = items.map((el, i) => ({ id: Number(el.dataset.id), sort_order: i + 1 }));
  items.forEach((el, i) => { el.dataset.sortOrder = i + 1; });
  try {
    await api(`/api/tracks/${trackId}/notes/order`, { method: 'PUT', body: JSON.stringify(payload) });
  } catch (e) { console.error('Failed to save note order:', e); }
}

// ─── Playback-bar note quick-access popover ──────────────────────────────────

function togglePbNotePopover(e) {
  if (e) e.stopPropagation();
  if (!state.playingTrackId) return;
  if (state.pbNotePopoverOpen) closePbNotePopover();
  else openPbNotePopover();
}

async function openPbNotePopover() {
  if (!state.playingTrackId) return;
  const btn = document.getElementById('btn-pb-note');
  const popover = document.getElementById('pb-note-popover');
  if (!btn || !popover) return;

  const title = document.getElementById('pb-track-name')?.textContent || '';
  if (!state.pbNoteTrack || state.pbNoteTrack.id !== state.playingTrackId) {
    state.pbNoteTrack = { id: state.playingTrackId, title, notes: [] };
  } else {
    state.pbNoteTrack.title = title;
  }
  const track = state.pbNoteTrack;

  state.pbNotePopoverOpen = true;
  document.getElementById('pb-note-wrap')?.classList.add('open');
  btn.classList.add('open');
  popover.classList.add('open');
  popover.setAttribute('aria-hidden', 'false');
  document.getElementById('pb-note-title-text').textContent = track.title || '—';
  renderPbNoteList(track);
  wirePbNoteAddInput(track);

  // Fetch fresh notes; ignore if a different track is selected by the time it lands
  try {
    const fresh = await api(`/api/tracks/${track.id}/notes`);
    if (state.pbNotePopoverOpen && state.pbNoteTrack && state.pbNoteTrack.id === track.id) {
      state.pbNoteTrack.notes = fresh;
      renderPbNoteList(state.pbNoteTrack);
      updatePbNoteBadge();
    }
  } catch (_) {}

  // Defer global listeners so the originating click doesn't immediately re-close
  setTimeout(() => {
    document.addEventListener('click', onPbNotePopoverDocClick, true);
    document.addEventListener('keydown', onPbNotePopoverKeydown);
  }, 0);
}

function closePbNotePopover() {
  const btn = document.getElementById('btn-pb-note');
  const popover = document.getElementById('pb-note-popover');
  if (popover) {
    popover.classList.remove('open');
    popover.setAttribute('aria-hidden', 'true');
  }
  if (btn) btn.classList.remove('open');
  document.getElementById('pb-note-wrap')?.classList.remove('open');
  state.pbNotePopoverOpen = false;
  document.removeEventListener('click', onPbNotePopoverDocClick, true);
  document.removeEventListener('keydown', onPbNotePopoverKeydown);
}

function onPbNotePopoverDocClick(e) {
  const popover = document.getElementById('pb-note-popover');
  const btn = document.getElementById('btn-pb-note');
  if (!popover) return;
  if (popover.contains(e.target) || (btn && btn.contains(e.target))) return;
  closePbNotePopover();
}

function onPbNotePopoverKeydown(e) {
  if (e.key === 'Escape') closePbNotePopover();
}

function renderPbNoteList(track) {
  const listEl = document.getElementById('pb-note-list');
  const emptyEl = document.getElementById('pb-note-empty');
  if (!listEl) return;
  renderNotesList(track, listEl);
  if (emptyEl) emptyEl.style.display = (track.notes || []).length ? 'none' : '';
}

// Rebind the popover's add-note input/button on each open (cheaper than tracking listeners)
function wirePbNoteAddInput(track) {
  const oldBtn = document.getElementById('pb-note-add-btn');
  const oldInput = document.getElementById('pb-note-input');
  if (!oldBtn || !oldInput) return;
  const addBtn = oldBtn.cloneNode(true);
  oldBtn.parentNode.replaceChild(addBtn, oldBtn);
  const input = oldInput.cloneNode(true);
  oldInput.parentNode.replaceChild(input, oldInput);

  addBtn.addEventListener('click', () => {
    addBtn.style.display = 'none';
    input.style.display = '';
    input.focus();
  });
  input.addEventListener('keydown', e => {
    e.stopPropagation();
    if (e.key === 'Escape') {
      input.style.display = 'none';
      addBtn.style.display = '';
      input.value = '';
    }
  });
  input.addEventListener('keyup', async e => {
    if (e.key !== 'Enter') return;
    const note = input.value.trim();
    if (!note) return;
    input.value = '';
    input.style.display = 'none';
    addBtn.style.display = '';
    await addTrackNote(track.id, note);
  });
}

// Show the badge when the playing track has at least one uncompleted note.
function updatePbNoteBadge() {
  const btn = document.getElementById('btn-pb-note');
  if (!btn) return;
  if (!state.playingTrackId) {
    btn.classList.remove('has-notes');
    btn.setAttribute('disabled', '');
    return;
  }
  btn.removeAttribute('disabled');
  const notes = (state.pbNoteTrack && state.pbNoteTrack.id === state.playingTrackId)
    ? (state.pbNoteTrack.notes || [])
    : null;
  if (notes === null) return;  // cache not warmed yet — refreshPbNoteCache() will retrigger
  btn.classList.toggle('has-notes', notes.some(n => !n.completed));
}

// Fetch notes for the currently playing track to warm the cache + badge.
async function refreshPbNoteCache() {
  const trackId = state.playingTrackId;
  if (!trackId) {
    state.pbNoteTrack = null;
    updatePbNoteBadge();
    return;
  }
  try {
    const notes = await api(`/api/tracks/${trackId}/notes`);
    if (state.playingTrackId !== trackId) return;
    const title = document.getElementById('pb-track-name')?.textContent || '';
    if (!state.pbNoteTrack || state.pbNoteTrack.id !== trackId) {
      state.pbNoteTrack = { id: trackId, title, notes };
    } else {
      state.pbNoteTrack.notes = notes;
      state.pbNoteTrack.title = title;
    }
    updatePbNoteBadge();
    if (state.pbNotePopoverOpen) {
      const titleEl = document.getElementById('pb-note-title-text');
      if (titleEl) titleEl.textContent = title || '—';
      renderPbNoteList(state.pbNoteTrack);
    }
  } catch (_) {}
}

function renderTrackList(tracks, crate) {
  const list = document.getElementById('tracks-list');
  if (!tracks.length) {
    list.innerHTML = `<div style="padding:24px;color:var(--muted);font-family:'IBM Plex Mono',monospace;font-size:11px;">No tracks found</div>`;
    return;
  }
  list.innerHTML = tracks.map((t, i) => {
    const isPlaying        = t.id === state.playingTrackId;
    const isSelected       = t.id === state.selectedTrackId;
    const isActivelyPlaying = isPlaying && state.playing;
    const playIcon         = isActivelyPlaying ? '⏸' : '▶';
    return `
      <div class="track-row${isPlaying ? ' playing' : ''}${isActivelyPlaying ? ' active-playing' : ''}${isSelected ? ' selected' : ''}"
           id="track-row-${t.id}" draggable="true">
        <div class="track-num">
          <span class="track-num-text">${String(i + 1).padStart(2, '0')}</span>
          <span class="track-eq-bars" aria-hidden="true"><span class="eq-b"></span><span class="eq-b"></span><span class="eq-b"></span></span>
          <button class="track-play-btn"
                  onclick="event.stopPropagation(); toggleTrackPlay(${i})">${playIcon}</button>
        </div>
        <div class="track-name">${escHtml(t.title)}</div>
        <div class="track-tags">${buildTrackTagsInnerHTML(t)}</div>
        <div class="track-dur">${t.duration ? formatTime(t.duration) : '—'}</div>
        <div class="track-star ${t.favorited ? 'starred' : ''}"
             onclick="toggleFavorite(event, ${t.id}, this)">★</div>
        <div class="track-notes-count" onclick="toggleTrackInspector(${i})">${
          (t.notes && t.notes.length)
            ? `<svg width="12" height="12" viewBox="0 0 14 14" fill="none" aria-hidden="true"><rect x="1.5" y="1.5" width="11" height="11" rx="1" stroke="currentColor" stroke-width="1.2"/><path d="M4 5h6M4 7.5h6M4 10h3.5" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/></svg>${t.notes.length}`
            : `<span class="notes-add-hint">+</span>`
        }</div>
      </div>`;
  }).join('');
  attachDragHandlers(list, crate);
  applyTagOverflowToAllRows();
}

// ─── Drag and Drop Track Reorder ──────────────────────────────────────────────

function clearDragIndicators(listEl) {
  listEl.querySelectorAll('.drag-over-top, .drag-over-bottom').forEach(el => {
    el.classList.remove('drag-over-top', 'drag-over-bottom');
  });
}

function attachDragHandlers(listEl, crate) {
  const rows = Array.from(listEl.querySelectorAll('.track-row'));

  // Clear stale module-level drag state from any previous render
  dragSrcIndex = null;
  dragOverIndex = null;
  dragOverPos = null;

  rows.forEach((row, index) => {
    row.addEventListener('dragstart', e => {
      dragSrcIndex = index;
      row.classList.add('dragging');
      e.dataTransfer.effectAllowed = 'move';
      e.dataTransfer.setData('text/plain', String(index)); // required for Firefox
    });

    row.addEventListener('dragend', () => {
      row.classList.remove('dragging');
      clearDragIndicators(listEl);
      dragSrcIndex  = null;
      dragOverIndex = null;
      dragOverPos   = null;
    });

    row.addEventListener('dragover', e => {
      e.preventDefault();
      if (dragSrcIndex === null || dragSrcIndex === index) return;
      const rect = row.getBoundingClientRect();
      const pos  = e.clientY < rect.top + rect.height / 2 ? 'top' : 'bottom';
      if (dragOverIndex !== index || dragOverPos !== pos) {
        clearDragIndicators(listEl);
        dragOverIndex = index;
        dragOverPos   = pos;
        row.classList.add(pos === 'top' ? 'drag-over-top' : 'drag-over-bottom');
      }
      e.dataTransfer.dropEffect = 'move';
    });

    row.addEventListener('dragleave', e => {
      // Only clear when leaving the list entirely, not when crossing into a sibling
      if (!listEl.contains(e.relatedTarget)) {
        clearDragIndicators(listEl);
        dragOverIndex = null;
        dragOverPos   = null;
      }
    });

    row.addEventListener('drop', e => {
      e.preventDefault();
      if (dragSrcIndex === null || dragSrcIndex === index) {
        clearDragIndicators(listEl);
        return;
      }

      const dragged = state.tracks[dragSrcIndex];
      const rest    = state.tracks.filter((_, i) => i !== dragSrcIndex);

      // Compute insertion point in the trimmed array
      let effectiveIndex;
      if (dragOverPos === 'top') {
        effectiveIndex = dragSrcIndex < index ? index - 1 : index;
      } else {
        effectiveIndex = dragSrcIndex < index ? index : index + 1;
      }
      rest.splice(effectiveIndex, 0, dragged);
      state.tracks = rest;

      clearDragIndicators(listEl);
      renderTrackList(state.tracks, crate);
      saveTrackOrder(crate.id, state.tracks.map(t => t.id));
    });
  });

  // Clear indicators when drag exits the list container
  listEl.addEventListener('dragleave', e => {
    if (!listEl.contains(e.relatedTarget)) {
      clearDragIndicators(listEl);
      dragOverIndex = null;
      dragOverPos   = null;
    }
  });
}

async function saveTrackOrder(crateId, orderedIds) {
  try {
    await api(`/api/crates/${crateId}/tracks/order`, {
      method: 'PUT',
      body: JSON.stringify({ order: orderedIds }),
    });
  } catch (err) {
    console.error('Failed to save track order:', err);
  }
}

// Play/pause a track from the detail view play button
function toggleTrackPlay(index) {
  const track = state.tracks[index];
  if (state.playingTrackId === track.id) {
    togglePlay();
    return;
  }
  jukeboxActive = false;
  jukeboxUpdatePbIcon();
  hwQueueActive = false;
  state.playingQueue = state.tracks.slice();
  state.playingIndex = index;
  state.playingCrate = state.activeCrate;
  loadAndPlay(track, state.activeCrate);
}

// Toggle inspector for a track row — opens if closed or if a different track was selected;
// closes if the inspector is already open for this track.
function toggleTrackInspector(index) {
  const track = state.tracks[index];
  if (state.selectedTrackId === track.id && state.inspectorOpen) {
    closeInspector();
    return;
  }
  openTrackInspector(index);
}

// Open the inspector for a track row (sole trigger: clicking the notes cell)
async function openTrackInspector(index) {
  const track = state.tracks[index];
  state.selectedTrackId = track.id;
  document.querySelectorAll('#tracks-list .track-row').forEach((r, i) =>
    r.classList.toggle('selected', i === index)
  );
  if (!state.inspectorOpen) toggleInspector();
  renderInspector(track); // render immediately with cached notes — no flash
  // Re-fetch notes in case they changed elsewhere (plugin polling, another tab)
  try {
    const freshNotes = await api(`/api/tracks/${track.id}/notes`);
    if (state.selectedTrackId === track.id) {
      track.notes = freshNotes;
      renderNotesList(track, document.getElementById('inspector-notes-list'));
    }
  } catch (_) {}
}

// ─── Playback ─────────────────────────────────────────────────────────────────

async function loadAndPlay(track, crate, startOffset = 0) {
  // Stop any currently playing node immediately so there is no overlap
  if (currentAudioNode) {
    currentAudioNode.onended = null;
    try { currentAudioNode.stop(); } catch (_) {}
    currentAudioNode.disconnect();
    currentAudioNode = null;
  }
  cancelAnimationFrame(pbTimeRAF);
  pbStartOffset = startOffset > 0 ? startOffset : 0;
  state.playing = false;

  // Mark this track as the one being loaded before any await
  state.playingTrackId = track.id;
  refreshPbNoteCache();
  invoke('log_play', { id: track.id })
    .then(() => refreshStats())
    .catch(() => {});

  // Update UI immediately (title, art, tags)
  document.getElementById('pb-track-name').textContent = track.title;
  document.getElementById('pb-crate-name').textContent =
    crate ? crate.name : (track.crate_name || '');
  if (crate) setPbArt(crate);
  else if (track.crate_id) setPbArt({ id: track.crate_id });
  renderPbTags(track);
  updatePbStar();
  document.getElementById('pb-track-info').classList.toggle('active', !!crate || !!track.crate_id);
  document.getElementById('btn-play').textContent = '⏸';
  refreshDetailPlayState();
  jukeboxSyncWidget();

  // Fetch and decode into an AudioBuffer if not already cached
  let buffer = audioBufferCache.get(track.id);
  if (!buffer) {
    try {
      const audioUrl = convertFileSrc(await invoke('track_audio_path', { id: track.id }));
      const res = await fetch(audioUrl);
      const arrayBuf = await res.arrayBuffer();
      buffer = await audioCtx.decodeAudioData(arrayBuf);
      cacheAudioBuffer(track.id, buffer);
    } catch (e) {
      // Decode failed (file missing/moved on disk, unsupported codec). Reset the
      // transport UI so it doesn't show a phantom "playing" state with no audio,
      // and tell the user why nothing played (M7 surfaces a "file not found").
      console.error('Audio decode failed for track', track.id, e);
      state.playing = false;
      document.getElementById('btn-play').textContent = '▶';
      setVinylSpin(false);
      cancelAnimationFrame(pbTimeRAF);
      refreshDetailPlayState();
      jukeboxSyncWidget();
      showToast(`Couldn't play "${track.title || 'this beat'}" — the file may have moved. Try Re-scan Library.`, 'err');
      return;
    }
  }

  // Another track may have been requested while we were decoding — abort if so
  if (state.playingTrackId !== track.id) return;

  // Resume AudioContext if suspended (browser autoplay policy)
  if (audioCtx.state === 'suspended') await audioCtx.resume();

  // Update duration display from decoded buffer
  pbDuration = buffer.duration;
  document.getElementById('pb-duration').textContent = '-' + formatTime(pbDuration);

  // Create and start source node — buffer is fully decoded, playback is clean.
  // Honor a pending seek offset (set while paused on a not-yet-decoded track); clamp it
  // to the real decoded duration so a seek past the end just restarts from 0.
  if (!(startOffset > 0) || startOffset >= buffer.duration - 0.1) pbStartOffset = 0;
  else pbStartOffset = startOffset;
  currentAudioNode = audioCtx.createBufferSource();
  currentAudioNode.buffer = buffer;
  currentAudioNode.connect(normGainNode);
  currentAudioNode.onended = onTrackEnded;
  applyNormGain(track);
  pbStartTime = audioCtx.currentTime;
  currentAudioNode.start(0, pbStartOffset);

  state.playing = true;
  document.getElementById('btn-play').textContent = '⏸';
  setVinylSpin(true);

  startMediaFocus();
  updateMediaSession(track, crate);

  refreshDetailPlayState();
  jukeboxSyncWidget();

  // Start position update loop
  startPbTimeUpdate();

  // Pre-decode the next track in the queue so its start is also clean
  const nextIdx = state.playingIndex + 1;
  if (nextIdx < state.playingQueue.length) {
    prefetchAudioBuffer(state.playingQueue[nextIdx].id);
  }
}

// Fetch and decode a track's audio into the cache in the background
function prefetchAudioBuffer(trackId) {
  if (audioBufferCache.has(trackId)) return;
  invoke('track_audio_path', { id: trackId })
    .then(p => fetch(convertFileSrc(p)))
    .then(r => r.arrayBuffer())
    .then(ab => audioCtx.decodeAudioData(ab))
    .then(buf => cacheAudioBuffer(trackId, buf))
    .catch(() => {});
}

function applyNormGain(track) {
  if (state.normalizationEnabled && track && track.replay_gain != null) {
    normGainNode.gain.value = Math.pow(10, track.replay_gain / 20);
  } else {
    normGainNode.gain.value = 1.0;
  }
}

function setVinylSpin(playing) {
  const icon = document.querySelector('.titlebar-logo-icon');
  if (icon) icon.classList.toggle('spinning', playing);
}

// Sync .playing/.active-playing classes and play button icons for the detail tracklist
function refreshDetailPlayState() {
  document.querySelectorAll('#tracks-list .track-row').forEach(row => {
    const trackId = parseInt(row.id.replace('track-row-', ''));
    const isActive = trackId === state.playingTrackId;
    row.classList.toggle('playing', isActive);
    row.classList.toggle('active-playing', isActive && state.playing);
    const btn = row.querySelector('.track-play-btn');
    if (btn) btn.textContent = (isActive && state.playing) ? '⏸' : '▶';
  });
  document.querySelectorAll('#flat-tracks-list .track-row').forEach(row => {
    const trackId = parseInt(row.id.replace('flat-track-', ''));
    const isActive = trackId === state.playingTrackId;
    row.classList.toggle('playing', isActive);
    row.classList.toggle('active-playing', isActive && state.playing);
    const btn = row.querySelector('.track-play-btn');
    if (btn) btn.textContent = (isActive && state.playing) ? '⏸' : '▶';
  });
  refreshCrateGridPlayState();
  refreshHwPlayState();
  refreshSongsPlayState();
}

async function togglePlay() {
  if (!state.playingTrackId) return;
  if (state.playing) {
    // Pause: save playback position, stop source node
    pbStartOffset += audioCtx.currentTime - pbStartTime;
    if (currentAudioNode) {
      currentAudioNode.onended = null;
      try { currentAudioNode.stop(); } catch (_) {}
      currentAudioNode.disconnect();
      currentAudioNode = null;
    }
    cancelAnimationFrame(pbTimeRAF);
    state.playing = false;
    document.getElementById('btn-play').textContent = '▶';
    setVinylSpin(false);
    if ('mediaSession' in navigator) navigator.mediaSession.playbackState = 'paused';
    pauseMediaFocus();
  } else {
    // Resume: create a new source node starting from saved offset
    const buffer = audioBufferCache.get(state.playingTrackId);
    if (!buffer) {
      // Buffer not in cache (track was cued without loading) — fetch and play, honoring
      // any seek the user performed while paused (otherwise the seek would be discarded).
      const track = state.playingQueue[state.playingIndex];
      if (track) loadAndPlay(track, state.playingCrate, pbStartOffset);
      return;
    }
    if (audioCtx.state === 'suspended') await audioCtx.resume();
    currentAudioNode = audioCtx.createBufferSource();
    currentAudioNode.buffer = buffer;
    currentAudioNode.connect(normGainNode);
    currentAudioNode.onended = onTrackEnded;
    applyNormGain(state.playingQueue[state.playingIndex]);
    pbStartTime = audioCtx.currentTime;
    currentAudioNode.start(0, pbStartOffset);
    state.playing = true;
    document.getElementById('btn-play').textContent = '⏸';
    setVinylSpin(true);
    if ('mediaSession' in navigator) navigator.mediaSession.playbackState = 'playing';
    startMediaFocus();
    startPbTimeUpdate();
  }
  refreshDetailPlayState();
  jukeboxSyncWidget();
}

function updateMediaSession(track, crate) {
  if (!('mediaSession' in navigator)) return;
  const artist  = (state.profile && state.profile.name) ? state.profile.name : '';
  const album   = crate ? crate.name : (track.crate_name || '');
  const crateId = crate ? crate.id  : track.crate_id;
  const artwork = crateId
    ? [{ src: crateCoverUrl(crateId), sizes: '512x512', type: 'image/jpeg' }]
    : [];
  navigator.mediaSession.metadata = new MediaMetadata({
    title: track.title || '',
    artist,
    album,
    artwork,
  });
  navigator.mediaSession.playbackState = 'playing';
  if (pbDuration && 'setPositionState' in navigator.mediaSession) {
    try {
      navigator.mediaSession.setPositionState({
        duration: pbDuration,
        position: pbStartOffset,
        playbackRate: 1,
      });
    } catch (_) {}
  }
}

function skipTrack(dir) {
  if (!state.playingQueue.length) return;
  const next = state.playingIndex + dir;
  if (next < 0 || next >= state.playingQueue.length) return;
  state.playingIndex = next;

  const track = state.playingQueue[next];

  // Update flat list highlight when not in the detail view
  if (state.currentView !== 'detail') {
    document.querySelectorAll('#flat-tracks-list .track-row').forEach((r, i) =>
      r.classList.toggle('playing', i === next)
    );
  }
  // Detail list buttons/highlight updated by loadAndPlay → refreshDetailPlayState

  loadAndPlay(track, state.playingCrate);
}

function getSeekRatio(e) {
  const bar = document.getElementById('pb-seek');
  const rect = bar.getBoundingClientRect();
  return Math.max(0, Math.min(1, (e.clientX - rect.left) / rect.width));
}

function seekToRatio(ratio) {
  if (!pbDuration) return;
  const offset = ratio * pbDuration;
  document.getElementById('pb-seek-fill').style.width = `${ratio * 100}%`;
  document.getElementById('pb-current').textContent = formatTime(offset);
  document.getElementById('pb-duration').textContent = '-' + formatTime(Math.max(0, pbDuration - offset));
  if (state.playing) {
    // Stop current node and restart at new position
    if (currentAudioNode) {
      currentAudioNode.onended = null;
      try { currentAudioNode.stop(); } catch (_) {}
      currentAudioNode.disconnect();
      currentAudioNode = null;
    }
    const buffer = audioBufferCache.get(state.playingTrackId);
    if (!buffer) { pbStartOffset = offset; return; }
    currentAudioNode = audioCtx.createBufferSource();
    currentAudioNode.buffer = buffer;
    currentAudioNode.connect(normGainNode);
    currentAudioNode.onended = onTrackEnded;
    pbStartTime = audioCtx.currentTime;
    pbStartOffset = offset;
    currentAudioNode.start(0, pbStartOffset);
  } else {
    pbStartOffset = offset;
  }
}

function setPbArt(crate) {
  const pbArt = document.getElementById('pb-art');
  const color = crateColor(crate.id);
  pbArt.innerHTML = `<div class="pb-art-inner ${color}"><div class="pb-art-ring"></div></div>`;
  loadCoverArt(pbArt, crateCoverUrl(crate.id));
}

function renderPbTags(track) {
  const container = document.getElementById('pb-tags');
  const tags = (track.tags && track.tags.length) ? track.tags : [];
  container.innerHTML = `<div class="pb-tags-chips">${
    tags.map(t => `<span class="pb-pill pb-pill-red">${escHtml(t)}</span>`).join('')
  }</div>`;
  applyPbTagOverflow(container);
}

// Hide trailing pills + show a +M glass badge when the playback-bar tags
// don't fit in the available right-side width. Mirrors applyTagOverflow() for
// crate rows, but pills here are read-only so popover clones don't carry × handlers.
function applyPbTagOverflow(tagsEl) {
  if (!tagsEl) return;
  const chipsEl = tagsEl.querySelector('.pb-tags-chips');
  if (!chipsEl) return;
  const existing = tagsEl.querySelector('.pb-tag-overflow');
  if (existing) existing.remove();
  const pills = Array.from(chipsEl.children);
  pills.forEach(p => { p.style.display = ''; });
  if (chipsEl.scrollWidth <= chipsEl.clientWidth + 1) return;

  const overflowEl = document.createElement('span');
  overflowEl.className = 'pb-pill pb-pill-red pb-tag-overflow';
  const labelNode = document.createTextNode('+0');
  const popover = document.createElement('span');
  popover.className = 'pb-tag-overflow-popover';
  overflowEl.appendChild(labelNode);
  overflowEl.appendChild(popover);
  tagsEl.appendChild(overflowEl);

  let hiddenCount = 0;
  for (let i = pills.length - 1; i >= 0; i--) {
    if (chipsEl.scrollWidth <= chipsEl.clientWidth + 1) break;
    pills[i].style.display = 'none';
    hiddenCount++;
  }
  if (hiddenCount === 0) { overflowEl.remove(); return; }

  labelNode.nodeValue = '+' + hiddenCount;
  for (let i = pills.length - hiddenCount; i < pills.length; i++) {
    const clone = pills[i].cloneNode(true);
    clone.style.display = '';
    popover.appendChild(clone);
  }
}

// ─── Favorites ────────────────────────────────────────────────────────────────

function updatePbStar() {
  const starEl = document.getElementById('btn-pb-star');
  if (!starEl) return;
  const track = state.playingQueue?.find(t => t.id === state.playingTrackId);
  starEl.classList.toggle('starred', !!track?.favorited);
}

async function togglePbFavorite(e) {
  if (!state.playingTrackId) return;
  const starEl = document.getElementById('btn-pb-star');
  const track = state.playingQueue.find(t => t.id === state.playingTrackId);
  if (!track) return;
  await toggleFavorite(e, state.playingTrackId, starEl);
  const queued = state.playingQueue.find(t => t.id === state.playingTrackId);
  if (queued) queued.favorited = starEl.classList.contains('starred') ? 1 : 0;
  updatePbStar();
}

async function toggleFavorite(e, trackId, starEl) {
  e.stopPropagation();
  const nowStarred = !starEl.classList.contains('starred');
  starEl.classList.toggle('starred', nowStarred);
  await invoke('set_track_favorite', { id: trackId, favorited: nowStarred });
  // Mirror into every cached list that holds this track, so the Beats-view Favorites
  // filter (which reads songsData, not state.tracks) doesn't go stale until a refetch.
  const fav = nowStarred ? 1 : 0;
  const t = state.tracks.find(t => t.id === trackId);
  if (t) t.favorited = fav;
  if (songsData) { const s = songsData.find(t => t.id === trackId); if (s) s.favorited = fav; }
}


// ─── Search Modal ─────────────────────────────────────────────────────────────

const searchBackdrop = document.getElementById('search-backdrop');
const searchModalInput = document.getElementById('search-modal-input');
const searchResults = document.getElementById('search-results');

function openSearchModal() {
  searchBackdrop.classList.add('open');
  searchBackdrop.removeAttribute('aria-hidden');
  searchModalInput.value = '';
  searchResults.innerHTML = '';
  searchModalInput.focus();
}

function closeSearchModal() {
  searchBackdrop.classList.remove('open');
  searchBackdrop.setAttribute('aria-hidden', 'true');
  searchModalInput.value = '';
  searchResults.innerHTML = '';
}

// Backdrop click dismisses (but not clicks inside the card)
searchBackdrop.addEventListener('click', e => {
  if (e.target === searchBackdrop) closeSearchModal();
});

// Escape closes
document.addEventListener('keydown', e => {
  if (e.key === 'Escape' && searchBackdrop.classList.contains('open')) {
    closeSearchModal();
    return;
  }
  // Cmd+K opens (Meta on Mac, guard against browser default with preventDefault)
  if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
    e.preventDefault();
    if (searchBackdrop.classList.contains('open')) {
      closeSearchModal();
    } else {
      openSearchModal();
    }
  }
});

// Debounced search
let searchDebounceTimer = null;
searchModalInput.addEventListener('input', () => {
  clearTimeout(searchDebounceTimer);
  searchDebounceTimer = setTimeout(runSearch, 200);
});

async function runSearch() {
  const q = searchModalInput.value.trim();
  if (!q) { searchResults.innerHTML = ''; return; }

  const data = await api(`/api/search?q=${encodeURIComponent(q)}`);
  if (!data) return;

  const { crates, tracks, notes } = data;

  if (!crates.length && !tracks.length && !notes.length) {
    searchResults.innerHTML = `<div class="search-no-results">No results for &ldquo;${escHtml(q)}&rdquo;</div>`;
    return;
  }

  let html = '';

  if (crates.length) {
    html += `<div class="search-section-label">Crates</div>`;
    crates.forEach(c => {
      const tagsHtml = (c.tags && c.tags.length)
        ? `<div class="search-result-tags">${c.tags.map(t => `<span class="search-result-tag">${escHtml(t)}</span>`).join('')}</div>`
        : '';
      html += `<div class="search-result-item" data-type="crate" data-crate-id="${c.id}">
        <div class="search-result-name">${escHtml(c.name)}</div>
        ${tagsHtml}
      </div>`;
    });
  }

  if (crates.length && tracks.length) html += `<div class="search-divider"></div>`;

  if (tracks.length) {
    html += `<div class="search-section-label">Tracks</div>`;
    tracks.forEach(t => {
      const tagsHtml = (t.tags && t.tags.length)
        ? `<div class="search-result-tags">${t.tags.map(tag => `<span class="search-result-tag">${escHtml(tag)}</span>`).join('')}</div>`
        : '';
      html += `<div class="search-result-item" data-type="track" data-crate-id="${t.crate_id}" data-track-id="${t.track_id}">
        <div class="search-result-name">${escHtml(t.title)}</div>
        <div class="search-result-sub">${escHtml(t.crate_name)}</div>
        ${tagsHtml}
      </div>`;
    });
  }

  if ((crates.length || tracks.length) && notes.length) html += `<div class="search-divider"></div>`;

  if (notes.length) {
    html += `<div class="search-section-label">Notes</div>`;
    notes.forEach(n => {
      const snippet = n.note_text.length > 60 ? escHtml(n.note_text.slice(0, 60)) + '…' : escHtml(n.note_text);
      html += `<div class="search-result-item search-result-note" data-type="note" data-crate-id="${n.crate_id}" data-track-id="${n.track_id}">
        <div class="search-result-name search-note-text">${snippet}</div>
        <div class="search-result-sub">${escHtml(n.track_title)} <span class="search-note-crate">· ${escHtml(n.crate_name)}</span></div>
      </div>`;
    });
  }

  searchResults.innerHTML = html;
}

// Result click navigation
searchResults.addEventListener('click', async e => {
  const item = e.target.closest('.search-result-item');
  if (!item) return;

  closeSearchModal();

  const crateId = parseInt(item.dataset.crateId, 10);
  const type = item.dataset.type;

  await openCrateDetail(crateId);

  if (type === 'track' || type === 'note') {
    const trackId = parseInt(item.dataset.trackId, 10);
    const idx = state.tracks.findIndex(t => t.id === trackId);
    if (idx !== -1) openTrackInspector(idx);
  }
});

// ─── Repeat ───────────────────────────────────────────────────────────────────

// 0 = off, 1 = repeat song, 2 = repeat album
let repeatMode = 0;

function cycleRepeat() {
  repeatMode = (repeatMode + 1) % 3;
  updateRepeatBtn();
}

function updateRepeatBtn() {
  const btn = document.getElementById('btn-repeat');
  btn.classList.remove('on', 'one');
  if (repeatMode === 0) {
    btn.title = 'Repeat: off';
  } else if (repeatMode === 1) {
    btn.classList.add('on', 'one');
    btn.title = 'Repeat: song';
  } else {
    btn.classList.add('on');
    btn.title = 'Repeat: album';
  }
}

// ─── Volume control ───────────────────────────────────────────────────────────

const VOL_SVG = '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="M1 5.5L4.5 5.5L8 3L8 13L4.5 10.5L1 10.5Z" fill="currentColor"/><path d="M10 6.5 Q11.5 8 10 9.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/><path d="M11.5 4.5 Q14.5 8 11.5 11.5" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';
const VOL_MUTED_SVG = '<svg width="15" height="15" viewBox="0 0 16 16" fill="none" aria-hidden="true"><path d="M1 5.5L4.5 5.5L8 3L8 13L4.5 10.5L1 10.5Z" fill="currentColor"/><path d="M10 6L13 10M13 6L10 10" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>';

function toggleVolPopup(e) {
  e.stopPropagation();
  document.getElementById('pb-vol-wrap').classList.toggle('open');
}

document.addEventListener('click', e => {
  const wrap = document.getElementById('pb-vol-wrap');
  if (wrap && wrap.classList.contains('open') && !wrap.contains(e.target)) {
    wrap.classList.remove('open');
  }
});

document.getElementById('pb-vol-slider').addEventListener('input', e => {
  gainNode.gain.value = e.target.value / 100;
  const icon = document.getElementById('btn-volume');
  if (icon) icon.innerHTML = parseFloat(e.target.value) === 0 ? VOL_MUTED_SVG : VOL_SVG;
});

// ─── Playback bar wiring ──────────────────────────────────────────────────────

document.getElementById('btn-play').addEventListener('click', togglePlay);
document.getElementById('btn-prev').addEventListener('click', () => {
  if (jukeboxActive) jukeboxPrev();
  else skipTrack(-1);
});
document.getElementById('btn-next').addEventListener('click', () => {
  if (jukeboxActive) jukeboxSkip();
  else skipTrack(1);
});

// Hardware media keys (F7/F8/F9 on Mac) + macOS Control Center "Now Playing".
// Both play and pause actions just call togglePlay — Chromium may route either
// depending on its own view of playbackState, so toggling is always correct.
if ('mediaSession' in navigator) {
  const mkToggle = () => { if (state.playingTrackId) togglePlay(); };
  navigator.mediaSession.setActionHandler('play',  mkToggle);
  navigator.mediaSession.setActionHandler('pause', mkToggle);
  navigator.mediaSession.setActionHandler('previoustrack', () => {
    if (jukeboxActive) jukeboxPrev(); else skipTrack(-1);
  });
  navigator.mediaSession.setActionHandler('nexttrack', () => {
    if (jukeboxActive) jukeboxSkip(); else skipTrack(1);
  });
}

// Chromium drops the page from "media producer" status when Web Audio stops,
// which lets macOS hand media keys back to whichever app was previously active
// (typically Apple Music). A silent looping <audio> element preserves focus.
let _mediaFocusAudio = null;
function ensureMediaFocusAudio() {
  if (_mediaFocusAudio) return _mediaFocusAudio;
  const sampleRate = 8000;
  const length     = sampleRate;
  const buf        = new ArrayBuffer(44 + length);
  const v          = new DataView(buf);
  v.setUint32(0,  0x52494646, false); // "RIFF"
  v.setUint32(4,  36 + length, true);
  v.setUint32(8,  0x57415645, false); // "WAVE"
  v.setUint32(12, 0x666d7420, false); // "fmt "
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);           // PCM
  v.setUint16(22, 1, true);           // mono
  v.setUint32(24, sampleRate, true);
  v.setUint32(28, sampleRate, true);
  v.setUint16(32, 1, true);
  v.setUint16(34, 8, true);           // 8-bit
  v.setUint32(36, 0x64617461, false); // "data"
  v.setUint32(40, length, true);
  for (let i = 0; i < length; i++) v.setUint8(44 + i, 0x80); // 8-bit unsigned silence

  const url = URL.createObjectURL(new Blob([buf], { type: 'audio/wav' }));
  const a   = document.createElement('audio');
  a.src     = url;
  a.loop    = true;
  a.volume  = 1;            // data is digital silence; volume>0 keeps Chromium tracking it
  a.preload = 'auto';
  a.style.display = 'none';
  document.body.appendChild(a);
  _mediaFocusAudio = a;
  return a;
}
function startMediaFocus() {
  const a = ensureMediaFocusAudio();
  if (a.paused) a.play().catch(() => {});
}
// Pause the silent focus audio so macOS Now Playing reflects the paused state.
// Without this, Chromium sees the looping <audio> still playing and reports
// 'playing' to the OS regardless of mediaSession.playbackState, leaving the
// menu bar widget stuck on ⏸. The element stays on the page so the page
// retains media-producer status and F8 still routes here on resume.
function pauseMediaFocus() {
  if (_mediaFocusAudio && !_mediaFocusAudio.paused) _mediaFocusAudio.pause();
}


document.getElementById('btn-repeat').addEventListener('click', cycleRepeat);
document.getElementById('btn-volume').addEventListener('click', toggleVolPopup);

// Seek bar — click and drag
document.getElementById('pb-seek').addEventListener('mousedown', e => {
  isDraggingSeek = true;
  seekToRatio(getSeekRatio(e));
});
document.addEventListener('mousemove', e => {
  if (!isDraggingSeek) return;
  seekToRatio(getSeekRatio(e));
});
document.addEventListener('mouseup', () => {
  isDraggingSeek = false;
});

// ─── Utilities ────────────────────────────────────────────────────────────────

// Lazy-loads a cover image into el. Replaces el.innerHTML once the image loads.
// Pass an explicit html string when custom markup is needed alongside the <img>.
function loadCoverArt(el, url, html) {
  const img = new Image();
  img.onload = () => { if (el) el.innerHTML = html !== undefined ? html : `<img src="${url}" alt="">`; };
  img.src = url;
}

function formatTime(secs) {
  if (!secs || isNaN(secs)) return '0:00';
  const m = Math.floor(secs / 60);
  const s = Math.floor(secs % 60);
  return `${m}:${String(s).padStart(2, '0')}`;
}

function escHtml(str) {
  return String(str)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

// ─── Songs tag-filter pill clicks (H1: delegated, not inline) ─────────────────
// Bound once at top level (the popover-dismiss listener lives in loadApp, which
// can run twice via the onboarding path — binding here avoids a double-toggle).
document.addEventListener('click', e => {
  const pill = e.target.closest && e.target.closest('.songs-tag-popover-pill');
  if (!pill || pill.dataset.tag === undefined) return;
  const tag = pill.dataset.tag;
  toggleSongsTagFilter(tag, !songsTagFilters.includes(tag));
});

// ─── Settings popover dismiss ─────────────────────────────────────────────────

document.addEventListener('click', e => {
  const popover = document.getElementById('settings-popover');
  if (popover && popover.classList.contains('open')) {
    // Now a centered modal: clicks on backdrop (popover layer outside the card) close;
    // clicks on the card or the avatar trigger don't.
    if (!e.target.closest('.settings-card') &&
        !e.target.closest('#titlebar-avatar')) {
      closeSettings();
    }
  }
  const cvd = document.getElementById('cvd');
  if (cvd && cvd.classList.contains('open') && !e.target.closest('#cvd')) {
    cvd.classList.remove('open');
  }
  const songsAc = document.getElementById('songs-tag-ac');
  if (songsAc && !e.target.closest('#songs-tag-filter')) {
    songsAc.classList.remove('open');
  }
});

document.addEventListener('keydown', e => {
  if (e.key === 'Escape') {
    closeSettings();
    closeInspector();
    const cvd = document.getElementById('cvd');
    if (cvd) cvd.classList.remove('open');
  }
});

// ─── Boot ─────────────────────────────────────────────────────────────────────

// H6: init() makes unguarded API calls (config/profile/stats). A locked or
// corrupt DB would throw and leave the renderer half-drawn with no explanation.
// Wrap it: on failure show a full-screen error with a Retry button instead.
function showFatalInitError(err) {
  console.error('init() failed:', err);
  const existing = document.getElementById('fatal-init-error');
  if (existing) existing.remove();
  const detail = String((err && err.message) ? err.message : err);
  const ov = document.createElement('div');
  ov.id = 'fatal-init-error';
  ov.className = 'fatal-init-error';
  ov.innerHTML =
    `<div class="fatal-init-panel">
      <div class="fatal-init-title">BeatCrate couldn't start</div>
      <div class="fatal-init-msg">Your library couldn't be opened. It may be in use by another app or temporarily locked. Your data is safe — try again in a moment.</div>
      <div class="fatal-init-detail">${escHtml(detail)}</div>
      <button class="fatal-init-retry" id="fatal-init-retry">Try again</button>
    </div>`;
  document.body.appendChild(ov);
  document.getElementById('fatal-init-retry').addEventListener('click', () => {
    ov.remove();
    bootApp();
  });
}

function bootApp() {
  init().catch(showFatalInitError);
}

setupBackendNoticeListener();
bootApp();

