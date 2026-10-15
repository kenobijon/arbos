// Arbos tree avatars for arbos.life.
//
// Markup:  <span class="avatar" data-avatar="oak" data-color="teal" data-state="idle" aria-label="Oakley"></span>
//   data-avatar  character slug (file names under BASE: <slug>.svg, lottie/<slug>/{idle,working,reply}.json)
//   data-color   palette slug (see PALETTE) or a JSON object of role → hex, e.g. {"tree":"#4ade80","face":"#0a0a0a"}
//   data-state   idle | working (loops); hover or focus plays `reply` once, then returns to the state
//   data-static  present → never animate (static SVG only)
//
// Behaviour: the tinted static SVG is inlined first (that is also the final render under
// prefers-reduced-motion and when lottie-web fails). lottie-web (lottie_light, self-hosted) loads once,
// only when the first avatar is near the viewport and motion is allowed. Animations are created lazily,
// paused while offscreen, and at most MAX_ANIMATED run at the same time; the rest stay static.
//
// Retint: colours are set by item name (`nm`) on `fl`/`st` shape items before loadAnimation — roles are
// tree, tree_dark, tree_light, face, ring, bg (manifest.recolor). Static SVGs are retinted by class.
(function () {
  'use strict';
  var BASE = '/assets/avatars/trees/';
  var LIB = '/assets/js/lottie_light.min.js';
  var MAX_ANIMATED = 12;
  var ROLES = ['tree', 'tree_dark', 'tree_light', 'face', 'ring', 'bg'];

  // Palette: accent colours from the avatar manifest (`colors[]`), expressed in the new roles
  // (the tree carries the colour; the disc stays neutral). Override with ArbosAvatars.configure().
  var PALETTE = {
    mono:   { tree: '#ffffff', tree_dark: '#cfcfcf', tree_light: '#ffffff', face: '#0a0a0a', ring: '#ffffff', bg: '#000000' },
    red:    { tree: '#f87171', tree_dark: '#dc4b4b', tree_light: '#fca5a5', face: '#2a1210', ring: '#f87171', bg: '#2a1210' },
    orange: { tree: '#fb923c', tree_dark: '#e0701a', tree_light: '#fdba74', face: '#2b1708', ring: '#fb923c', bg: '#2b1708' },
    amber:  { tree: '#fbbf24', tree_dark: '#d99c08', tree_light: '#fde68a', face: '#2a1f08', ring: '#fbbf24', bg: '#2a1f08' },
    green:  { tree: '#4ade80', tree_dark: '#22b35a', tree_light: '#86efac', face: '#0f2a1a', ring: '#4ade80', bg: '#0f2a1a' },
    teal:   { tree: '#2dd4bf', tree_dark: '#14a89a', tree_light: '#5eead4', face: '#0b2826', ring: '#2dd4bf', bg: '#0b2826' },
    cyan:   { tree: '#22d3ee', tree_dark: '#08a9c4', tree_light: '#67e8f9', face: '#0b2430', ring: '#22d3ee', bg: '#0b2430' },
    blue:   { tree: '#60a5fa', tree_dark: '#3b82f6', tree_light: '#93c5fd', face: '#0f1f3a', ring: '#60a5fa', bg: '#0f1f3a' },
    indigo: { tree: '#818cf8', tree_dark: '#6366f1', tree_light: '#a5b4fc', face: '#171a3c', ring: '#818cf8', bg: '#171a3c' },
    violet: { tree: '#a78bfa', tree_dark: '#8b5cf6', tree_light: '#c4b5fd', face: '#221540', ring: '#a78bfa', bg: '#221540' },
    pink:   { tree: '#f472b6', tree_dark: '#ec4899', tree_light: '#f9a8d4', face: '#2a1230', ring: '#f472b6', bg: '#2a1230' },
    rose:   { tree: '#fb7185', tree_dark: '#f43f5e', tree_light: '#fda4af', face: '#2c1020', ring: '#fb7185', bg: '#2c1020' },
    slate:  { tree: '#94a3b8', tree_dark: '#64748b', tree_light: '#cbd5e1', face: '#1a1f2a', ring: '#94a3b8', bg: '#1a1f2a' }
  };

  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  var cache = {};          // url → Promise<string|object>
  var libPromise = null;   // Promise<lottie>
  var instances = [];      // live Avatar objects
  var animatedCount = 0;

  function fetchText(url) {
    if (!cache[url]) cache[url] = fetch(url, { credentials: 'same-origin' }).then(function (r) { if (!r.ok) throw new Error(url + ' ' + r.status); return r.text(); });
    return cache[url];
  }
  function fetchJson(url) {
    if (!cache[url]) cache[url] = fetch(url, { credentials: 'same-origin' }).then(function (r) { if (!r.ok) throw new Error(url + ' ' + r.status); return r.json(); });
    return cache[url];
  }
  function loadLib() {
    if (window.lottie) return Promise.resolve(window.lottie);
    if (!libPromise) libPromise = new Promise(function (resolve, reject) {
      var s = document.createElement('script');
      s.src = LIB; s.async = true;
      s.onload = function () { window.lottie ? resolve(window.lottie) : reject(new Error('lottie missing')); };
      s.onerror = function () { reject(new Error('lottie failed to load')); };
      document.head.appendChild(s);
    });
    return libPromise;
  }

  // ---- colours ----
  function hexToRgba(hex) {
    var h = hex.replace('#', '');
    if (h.length === 3) h = h.replace(/(.)/g, '$1$1');
    var n = parseInt(h, 16);
    return [((n >> 16) & 255) / 255, ((n >> 8) & 255) / 255, (n & 255) / 255, 1];
  }
  function resolveTint(spec) {
    if (!spec) return null;
    if (spec.charAt(0) === '{') { try { return JSON.parse(spec); } catch (e) { return null; } }
    return PALETTE[spec] || null;
  }

  // Walk every shape list (layers, groups, precomp assets) and recolour fl/st items by `nm`.
  function tintLottie(data, tint) {
    if (!tint) return data;
    var seen = 0;
    function items(list) {
      for (var i = 0; i < list.length; i++) {
        var it = list[i];
        if (it.ty === 'gr' && it.it) items(it.it);
        else if ((it.ty === 'fl' || it.ty === 'st') && it.c && tint[it.nm]) {
          var rgba = hexToRgba(tint[it.nm]);
          if (it.c.a === 1 && Array.isArray(it.c.k)) { it.c.k.forEach(function (kf) { if (kf.s) kf.s = rgba.slice(); if (kf.e) kf.e = rgba.slice(); }); }
          else it.c.k = rgba;
          seen++;
        }
      }
    }
    function layers(ls) { (ls || []).forEach(function (l) { if (l.shapes) items(l.shapes); }); }
    layers(data.layers);
    (data.assets || []).forEach(function (a) { layers(a.layers); });
    data.__tinted = seen;
    return data;
  }
  function tintSvg(svgText, tint) {
    var wrap = document.createElement('div');
    wrap.innerHTML = svgText.trim();
    var svg = wrap.firstElementChild;
    if (!svg || svg.tagName.toLowerCase() !== 'svg') return null;
    svg.removeAttribute('width'); svg.removeAttribute('height');
    svg.setAttribute('focusable', 'false');
    svg.setAttribute('aria-hidden', 'true');
    if (tint) ROLES.forEach(function (role) {
      if (!tint[role]) return;
      var nodes = svg.querySelectorAll('.' + role);
      for (var i = 0; i < nodes.length; i++) {
        var n = nodes[i];
        if (n.getAttribute('fill') && n.getAttribute('fill') !== 'none') n.setAttribute('fill', tint[role]);
        if (n.getAttribute('stroke') && n.getAttribute('stroke') !== 'none') n.setAttribute('stroke', tint[role]);
      }
    });
    return svg;
  }

  // ---- one avatar ----
  function Avatar(el) {
    this.el = el;
    this.slug = el.getAttribute('data-avatar');
    this.tint = resolveTint(el.getAttribute('data-color'));
    this.state = el.getAttribute('data-state') === 'working' ? 'working' : 'idle';
    this.anim = null;
    this.visible = false;
    this.animating = false;
    this.replying = false;
    this.wantReply = false;
    var self = this;
    el.classList.add('avatar');
    if (!el.hasAttribute('role')) el.setAttribute('role', 'img');
    this.renderStatic();
    if (!el.hasAttribute('data-static')) {
      el.addEventListener('mouseenter', function () { self.reply(); });
      el.addEventListener('focus', function () { self.reply(); });
    }
  }
  Avatar.prototype.renderStatic = function () {
    var self = this;
    fetchText(BASE + this.slug + '.svg').then(function (text) {
      var svg = tintSvg(text, self.tint);
      if (!svg || self.anim) return;
      self.el.replaceChildren(svg);
      self.el.classList.add('is-ready');
    }).catch(function () { self.el.classList.add('is-missing'); });
  };
  Avatar.prototype.canAnimate = function () {
    return !reduce.matches && !this.el.hasAttribute('data-static');
  };
  Avatar.prototype.setVisible = function (v) {
    this.visible = v;
    if (!this.canAnimate()) return;
    if (v) this.start(); else this.stop();
  };
  Avatar.prototype.start = function () {
    var self = this;
    if (this.anim) { if (!this.animating) { this.anim.play(); this.animating = true; animatedCount++; } return; }
    if (this.loading || animatedCount >= MAX_ANIMATED) return;
    this.loading = true;
    Promise.all([loadLib(), fetchJson(BASE + 'lottie/' + this.slug + '/' + this.state + '.json')]).then(function (res) {
      self.loading = false;
      if (!self.visible || !self.canAnimate() || animatedCount >= MAX_ANIMATED) return;
      self.mount(res[0], res[1], self.state, true);
    }).catch(function () { self.loading = false; });
  };
  Avatar.prototype.stop = function () {
    if (this.anim && this.animating) { this.anim.pause(); this.animating = false; animatedCount--; }
  };
  Avatar.prototype.mount = function (lottie, data, name, loop) {
    var self = this;
    var copy = tintLottie(JSON.parse(JSON.stringify(data)), this.tint);
    var holder = document.createElement('span');
    holder.className = 'avatar-anim';
    var anim = lottie.loadAnimation({ container: holder, renderer: 'svg', loop: loop, autoplay: true, animationData: copy,
      rendererSettings: { progressiveLoad: false, hideOnTransparent: true, className: 'avatar-svg' } });
    if (this.anim) { this.destroy(); }
    this.el.replaceChildren(holder);
    this.anim = anim; this.animating = true; animatedCount++;
    this.current = name;
    anim.addEventListener('complete', function () { if (!loop) self.afterReply(); });
    anim.addEventListener('loopComplete', function () { if (self.wantReply) { self.wantReply = false; self.playReply(); } });
    anim.addEventListener('data_failed', function () { self.destroy(); self.renderStatic(); });
  };
  Avatar.prototype.destroy = function () {
    if (!this.anim) return;
    if (this.animating) animatedCount--;
    this.anim.destroy(); this.anim = null; this.animating = false;
  };
  // Hover: finish the current loop (all states share frame 0), then play `reply` once.
  Avatar.prototype.reply = function () {
    if (!this.anim || this.replying || !this.canAnimate()) return;
    this.wantReply = true;
    fetchJson(BASE + 'lottie/' + this.slug + '/reply.json').catch(function () {});
  };
  Avatar.prototype.playReply = function () {
    var self = this;
    if (!this.visible || this.replying) return;
    this.replying = true;
    Promise.all([loadLib(), fetchJson(BASE + 'lottie/' + this.slug + '/reply.json')]).then(function (res) {
      if (!self.visible) { self.replying = false; return; }
      self.mount(res[0], res[1], 'reply', false);
    }).catch(function () { self.replying = false; });
  };
  Avatar.prototype.afterReply = function () {
    var self = this;
    this.replying = false;
    fetchJson(BASE + 'lottie/' + this.slug + '/' + this.state + '.json').then(function (data) {
      if (!self.anim) return;
      loadLib().then(function (lottie) { self.mount(lottie, data, self.state, true); if (!self.visible) self.stop(); });
    });
  };
  Avatar.prototype.setState = function (state) {
    state = state === 'working' ? 'working' : 'idle';
    if (state === this.state) return;
    this.state = state;
    this.el.setAttribute('data-state', state);
    if (this.anim && !this.replying) { var self = this; this.wantReply = false; this.anim.addEventListener('loopComplete', function once() { self.anim.removeEventListener('loopComplete', once); self.afterReply(); }); }
  };

  // ---- page wiring ----
  var io = null;
  function observe(av) {
    if (!('IntersectionObserver' in window)) { av.setVisible(true); return; }
    if (!io) io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) { var av = en.target.__avatar; if (av) av.setVisible(en.isIntersecting); });
    }, { rootMargin: '160px 0px' });
    av.el.__avatar = av;
    io.observe(av.el);
  }
  function mountAll(root) {
    var els = (root || document).querySelectorAll('[data-avatar]');
    for (var i = 0; i < els.length; i++) {
      if (els[i].__avatar) continue;
      var av = new Avatar(els[i]);
      instances.push(av);
      observe(av);
    }
  }
  reduce.addEventListener('change', function () {
    instances.forEach(function (av) { if (reduce.matches) { av.destroy(); av.renderStatic(); } else if (av.visible) av.start(); });
  });
  document.addEventListener('visibilitychange', function () {
    instances.forEach(function (av) { if (document.hidden) av.stop(); else if (av.visible) av.setVisible(true); });
  });

  window.ArbosAvatars = {
    mount: mountAll,
    tintLottie: tintLottie,
    tintSvg: tintSvg,
    palette: PALETTE,
    configure: function (opts) {
      if (opts.base) BASE = opts.base;
      if (opts.lib) LIB = opts.lib;
      if (opts.maxAnimated) MAX_ANIMATED = opts.maxAnimated;
      if (opts.palette) Object.keys(opts.palette).forEach(function (k) { PALETTE[k] = opts.palette[k]; });
    },
    setState: function (el, state) { if (el.__avatar) el.__avatar.setState(state); },
    stats: function () { return { total: instances.length, animated: animatedCount }; }
  };
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', function () { mountAll(); });
  else mountAll();
})();
