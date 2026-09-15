async ({system, action, payload = {}}) => {
  // Session credentials stay in the authenticated tab and never enter Python.
  try {
    let url, body, headers, method = 'POST';
    if (system === 'mithf') {
      if (location.origin !== 'https://mithf.handicapformidlingen.dk') throw 0;
      const allowed = ['plan', 'ekstra', 'hjaelperliste', 'muligheder', 'pulje',
        'opret', 'rettid', 'book', 'tilfoejreg', 'retreg'];
      if (!allowed.includes(action)) throw 0;
      const scripts = [...document.scripts].filter(s => !s.src).map(s => s.textContent).join('\n');
      const match = scripts.match(/var TOK=("[^"]*"|'[^']*')/);
      if (!match) return {error: 'MitHF session is missing; reload and sign in'};
      url = '/vagtplan/vagt-api.php';
      headers = {'Content-Type': 'application/x-www-form-urlencoded'};
      body = new URLSearchParams({...payload, a: action, csrf: match[1].slice(1, -1)});
    } else if (system === 'duos') {
      if (location.origin !== 'https://mit.duos.dk' || localStorage.getItem('role') !== 'citizen'
          || localStorage.getItem('onBehalfOfUserId')) throw 0;
      const routes = {
        search: ['POST', '/api/citizens/time-registrations/search'],
        portfolios: ['GET', '/api/citizens/time-registration/get-portfolios'],
        employments: ['GET', `/api/citizens/time-registration/${encodeURIComponent(payload.portfolioId)}/get-employments`],
        types: ['GET', `/api/citizens/time-registration/${encodeURIComponent(payload.portfolioId)}/get-registration-types`],
        detail: ['GET', `/api/citizens/time-registration/get-registration-by-id/${encodeURIComponent(payload.id)}`],
        register: ['POST', '/api/citizens/time-registration/register-hours'],
      };
      if (!routes[action]) throw 0;
      [method, url] = routes[action];
      const token = localStorage.getItem('token');
      if (!token) return {error: 'DUOS session is missing; sign in'};
      headers = {'Content-Type': 'application/json', 'X-Requested-With': 'XMLHttpRequest',
        Accept: 'application/json', Authorization: `Bearer ${token}`};
      if (method === 'GET') url += '?' + new URLSearchParams(payload);
      else body = JSON.stringify(payload);
    } else throw 0;
    const response = await fetch(url, {method, headers, body, redirect: 'error', cache: 'no-store',
      signal: AbortSignal.timeout(45000)});
    if (!response.ok) return {error: `${system} returned HTTP ${response.status}`};
    const data = response.status === 204 ? null : await response.json();
    if (system === 'mithf' && data?.ok !== true) return {error: 'MitHF rejected the request; inspect the app before retrying'};
    return {data};
  } catch {
    // A write may have reached the server. The executor checkpoints before sending.
    return {error: 'Destination request failed; its outcome must be reconciled before retrying'};
  }
}
