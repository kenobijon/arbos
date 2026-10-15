// /welcome — the landing after the GitHub OAuth redirect. Reads ?code (or ?token, ?error) and
// tells the user to open the Mac app. Nothing is sent anywhere from this page.
(function () {
  'use strict';
  var params = new URLSearchParams(window.location.search);
  var code = params.get('code') || params.get('token') || '';
  var error = params.get('error');
  var signedIn = document.querySelector('[data-state="signed-in"]');
  var noCode = document.querySelector('[data-state="no-code"]');

  if (!code || error) {
    signedIn.hidden = true;
    noCode.hidden = false;
    if (error) {
      var p = noCode.querySelector('.welcome-lede');
      var message = params.get('message') || error;
      p.textContent = 'GitHub sign-in did not finish: ' + message.replace(/[^a-z0-9_.,' -]/gi, '') + '. Try again from the app.';
    }
    return;
  }

  var box = document.getElementById('code-box');
  var codeEl = document.getElementById('code');
  var openApp = document.getElementById('open-app');
  codeEl.textContent = code;
  box.hidden = false;
  openApp.href = 'arbos://auth/callback?code=' + encodeURIComponent(code);
  // The sign-in code is single use; keep it out of the browser history once shown.
  if (window.history && history.replaceState) history.replaceState(null, '', window.location.pathname);

  var copy = document.getElementById('copy');
  copy.addEventListener('click', function () {
    if (!navigator.clipboard) return;
    navigator.clipboard.writeText(code).then(function () {
      copy.textContent = 'Copied';
      setTimeout(function () { copy.textContent = 'Copy'; }, 1500);
    });
  });
})();
