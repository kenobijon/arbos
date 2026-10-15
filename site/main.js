// arbos.life — progressive enhancement only. Without this file the page is complete and static.
(function () {
  'use strict';
  var doc = document;
  var root = doc.documentElement;
  var reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  root.classList.add('js');

  // ---- Header: background after scroll, mobile menu, download split menu ----
  var header = doc.querySelector('.site-header');
  function onScroll() { header.classList.toggle('is-scrolled', window.scrollY > 8); }
  onScroll();
  window.addEventListener('scroll', onScroll, { passive: true });

  var mobileToggle = doc.querySelector('[data-mobile-toggle]');
  var mobileMenu = doc.getElementById('mobile-menu');
  function setMobile(open) {
    mobileToggle.setAttribute('aria-expanded', String(open));
    mobileToggle.setAttribute('aria-label', open ? 'Close menu' : 'Open menu');
    mobileMenu.hidden = !open;
    header.classList.toggle('is-open', open);
  }
  if (mobileToggle && mobileMenu) {
    mobileToggle.addEventListener('click', function () { setMobile(mobileMenu.hidden); });
    mobileMenu.addEventListener('click', function (e) { if (e.target.closest('a')) setMobile(false); });
  }

  var menuWrap = doc.querySelector('[data-menu]');
  if (menuWrap) {
    var menuButton = menuWrap.querySelector('[data-menu-button]');
    var menuPanel = menuWrap.querySelector('[data-menu-panel]');
    var setMenu = function (open) { menuButton.setAttribute('aria-expanded', String(open)); menuPanel.hidden = !open; };
    menuButton.addEventListener('click', function () { setMenu(menuPanel.hidden); });
    doc.addEventListener('click', function (e) { if (!menuWrap.contains(e.target)) setMenu(false); });
    doc.addEventListener('keydown', function (e) { if (e.key === 'Escape') { setMenu(false); if (mobileMenu && !mobileMenu.hidden) setMobile(false); } });
  }

  // ---- Hero entrance ----
  function enter() {
    var words = doc.querySelector('[data-words]');
    var fades = doc.querySelectorAll('#hero [data-fade]');
    var frame = doc.querySelector('[data-frame]');
    var t = reduce ? 0 : 1;
    fades.forEach(function (el, i) { setTimeout(function () { el.classList.add('in'); }, t * (80 + i * 220)); });
    if (words) setTimeout(function () { words.classList.add('in'); }, t * 120);
    if (frame) setTimeout(function () { frame.classList.add('in'); }, t * 200);
  }
  if (doc.fonts && doc.fonts.ready) { doc.fonts.ready.then(enter, enter); } else { enter(); }

  // ---- Scroll reveal ----
  var reveals = doc.querySelectorAll('[data-reveal]');
  if ('IntersectionObserver' in window && !reduce) {
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (en) { if (en.isIntersecting) { en.target.classList.add('in'); io.unobserve(en.target); } });
    }, { rootMargin: '0px 0px -10% 0px', threshold: 0.08 });
    reveals.forEach(function (el) { io.observe(el); });
  } else {
    reveals.forEach(function (el) { el.classList.add('in'); });
  }

  // ---- Demo conversation: plays when visible, loops ----
  var demo = doc.querySelector('[data-demo]');
  if (demo) {
    var msgs = Array.prototype.slice.call(demo.querySelectorAll('.demo-msg'));
    var timer = null;
    var play = function () {
      msgs.forEach(function (m) { m.classList.remove('in'); });
      var i = 0;
      var step = function () {
        if (i < msgs.length) { msgs[i++].classList.add('in'); timer = setTimeout(step, i === 1 ? 900 : 1700); }
        else { timer = setTimeout(play, 5000); }
      };
      timer = setTimeout(step, 500);
    };
    if (reduce || !('IntersectionObserver' in window)) {
      msgs.forEach(function (m) { m.classList.add('in'); });
    } else {
      var dio = new IntersectionObserver(function (entries) {
        entries.forEach(function (en) {
          if (en.isIntersecting && timer === null) play();
          else if (!en.isIntersecting && timer !== null) { clearTimeout(timer); timer = null; }
        });
      }, { threshold: 0.35 });
      dio.observe(demo);
    }
  }

  // ---- Use-case chips ----
  var chips = doc.querySelector('[data-chips]');
  if (chips) {
    var tabs = Array.prototype.slice.call(chips.querySelectorAll('[data-chip]'));
    var panels = Array.prototype.slice.call(doc.querySelectorAll('[data-panel]'));
    var title = doc.querySelector('[data-thread-title]');
    var select = function (name) {
      tabs.forEach(function (b) {
        var on = b.getAttribute('data-chip') === name;
        b.classList.toggle('is-active', on);
        b.setAttribute('aria-selected', String(on));
      });
      panels.forEach(function (p) { p.hidden = p.getAttribute('data-panel') !== name; });
      var active = tabs.filter(function (b) { return b.classList.contains('is-active'); })[0];
      if (title && active) title.textContent = active.textContent.trim();
      // The thread card's avatar follows the selected project (the avatar loader remounts new spans).
      if (active && window.ArbosAvatars) {
        var slug = active.getAttribute('data-avatar-slug'), color = active.getAttribute('data-avatar-color');
        doc.querySelectorAll('.thread-avatar .avatar, [data-thread-msg-avatar]').forEach(function (old) {
          if (old.getAttribute('data-avatar') === slug) return;
          var fresh = doc.createElement('span');
          fresh.className = old.className.replace(/\bis-ready\b|\bis-missing\b/g, '').trim();
          fresh.setAttribute('data-avatar', slug); fresh.setAttribute('data-color', color);
          fresh.setAttribute('aria-label', active.textContent.trim());
          if (old.hasAttribute('data-thread-msg-avatar')) fresh.setAttribute('data-thread-msg-avatar', '');
          if (old.__avatar) old.__avatar.destroy();
          old.replaceWith(fresh);
          window.ArbosAvatars.mount(fresh.parentNode);
        });
      }
    };
    chips.addEventListener('click', function (e) { var b = e.target.closest('[data-chip]'); if (b) select(b.getAttribute('data-chip')); });
    chips.addEventListener('keydown', function (e) {
      if (e.key !== 'ArrowRight' && e.key !== 'ArrowLeft') return;
      var i = tabs.indexOf(doc.activeElement); if (i < 0) return;
      var n = tabs[(i + (e.key === 'ArrowRight' ? 1 : tabs.length - 1)) % tabs.length];
      n.focus(); select(n.getAttribute('data-chip')); e.preventDefault();
    });
  }

  // ---- FAQ: one open at a time, animated height ----
  var acc = doc.querySelector('[data-accordion]');
  if (acc) {
    var items = Array.prototype.slice.call(acc.querySelectorAll('details'));
    items.forEach(function (d) {
      var body = d.querySelector('.faq-body');
      var summary = d.querySelector('summary');
      summary.addEventListener('click', function (e) {
        e.preventDefault();
        if (d.open) { close(d); }
        else { items.forEach(function (o) { if (o !== d && o.open) close(o); }); open(d); }
      });
      function open(el) {
        el.open = true;
        if (reduce) return;
        var h = body.scrollHeight;
        body.style.height = '0px';
        body.style.transition = 'height 0.35s cubic-bezier(.25,1,.5,1)';
        requestAnimationFrame(function () { body.style.height = h + 'px'; });
        body.addEventListener('transitionend', function te() { body.style.height = ''; body.style.transition = ''; body.removeEventListener('transitionend', te); });
      }
      function close(el) {
        var b = el.querySelector('.faq-body');
        if (reduce) { el.open = false; return; }
        b.style.height = b.scrollHeight + 'px';
        b.style.transition = 'height 0.3s cubic-bezier(.25,1,.5,1)';
        requestAnimationFrame(function () { b.style.height = '0px'; });
        b.addEventListener('transitionend', function te() { el.open = false; b.style.height = ''; b.style.transition = ''; b.removeEventListener('transitionend', te); });
      }
    });
  }
})();
