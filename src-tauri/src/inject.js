(function () {
  if (window.__rever_inject_done) return;
  window.__rever_inject_done = true;

  function send(payload) {
    try {
      fetch('reverevt://event', {
        method: 'POST',
        body: JSON.stringify(payload),
        keepalive: true,
        credentials: 'omit',
        mode: 'no-cors'
      }).catch(function () {});
    } catch (e) {}
  }

  function newId() {
    if (window.crypto && crypto.randomUUID) return crypto.randomUUID();
    return 'r-' + Date.now() + '-' + Math.random().toString(36).slice(2, 10);
  }

  function ts() {
    return performance.now() / 1000;
  }

  // ─── fetch ───
  var origFetch = window.fetch;
  window.fetch = function (input, init) {
    var id = newId();
    var url = typeof input === 'string' ? input : input && input.url ? input.url : '';
    var method = (init && init.method) || (input && typeof input !== 'string' && input.method) || 'GET';

    send({
      type: 'request',
      request_id: id,
      url: url,
      method: method.toUpperCase(),
      resource_type: 'fetch',
      timestamp: ts()
    });

    return origFetch.call(this, input, init).then(
      function (res) {
        var contentLen = parseInt(res.headers.get('content-length') || '0', 10) || 0;
        send({
          type: 'response',
          request_id: id,
          status: res.status,
          mime_type: res.headers.get('content-type') || '',
          timestamp: ts()
        });
        send({
          type: 'finished',
          request_id: id,
          encoded_data_length: contentLen,
          timestamp: ts()
        });
        return res;
      },
      function (err) {
        send({
          type: 'finished',
          request_id: id,
          encoded_data_length: 0,
          timestamp: ts()
        });
        throw err;
      }
    );
  };

  // ─── XMLHttpRequest ───
  var OrigXHR = window.XMLHttpRequest;
  function HookedXHR() {
    var xhr = new OrigXHR();
    var id = null;
    var url = '';
    var method = 'GET';

    var origOpen = xhr.open;
    xhr.open = function (m, u) {
      method = m;
      url = u;
      id = newId();
      return origOpen.apply(this, arguments);
    };

    var origSend = xhr.send;
    xhr.send = function () {
      send({
        type: 'request',
        request_id: id,
        url: url,
        method: method.toUpperCase(),
        resource_type: 'xhr',
        timestamp: ts()
      });

      xhr.addEventListener('load', function () {
        var len = parseInt(xhr.getResponseHeader('content-length') || '0', 10) || 0;
        send({
          type: 'response',
          request_id: id,
          status: xhr.status,
          mime_type: xhr.getResponseHeader('content-type') || '',
          timestamp: ts()
        });
        send({
          type: 'finished',
          request_id: id,
          encoded_data_length: len,
          timestamp: ts()
        });
      });

      xhr.addEventListener('error', function () {
        send({
          type: 'finished',
          request_id: id,
          encoded_data_length: 0,
          timestamp: ts()
        });
      });

      return origSend.apply(this, arguments);
    };

    return xhr;
  }
  HookedXHR.prototype = OrigXHR.prototype;
  window.XMLHttpRequest = HookedXHR;
})();
