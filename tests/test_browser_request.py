"""Execute the browser request script with a simulated MitHF page and fetch.

Node supplies browser-compatible fetch primitives; no service or credentials
are used. These checks are skipped where Node is unavailable.
"""

import json
import shutil
import subprocess
import unittest

from teamup_shift_sync.browser import REQUEST_SCRIPT


@unittest.skipUnless(shutil.which("node"), "Browser-script checks require Node")
class MitHfLoginTests(unittest.TestCase):
    def probe(self, *, scripts=""):
        harness = r"""
const fs = require('node:fs');
const {script, scripts} = JSON.parse(fs.readFileSync(0, 'utf8'));
global.location = {origin: 'https://mithf.handicapformidlingen.dk'};
global.document = {scripts: [{src: '', textContent: scripts}]};
const calls = [];
global.fetch = async (url, options) => {
  calls.push({url, method: options.method || 'GET', redirect: options.redirect,
              csrf: options.body?.get('csrf'), action: options.body?.get('a')});
  if (url === '/vagtplan/vagt-api.php') {
    return {ok: true, status: 200, json: async () => ({ok: true, grupper: []})};
  }
  throw new Error('Unexpected endpoint');
};
(async () => {
  const result = await eval('(' + script + ')')({system: 'mithf', action: 'hjaelperliste'});
  process.stdout.write(JSON.stringify({result, calls}));
})();
"""
        completed = subprocess.run(
            [shutil.which("node"), "-e", harness],
            input=json.dumps(
                {
                    "script": REQUEST_SCRIPT,
                    "scripts": scripts,
                }
            ),
            text=True,
            capture_output=True,
            check=True,
            timeout=10,
        )
        return json.loads(completed.stdout)

    def test_page_without_calendar_token_does_not_open_calendar_directly(self):
        result = self.probe()
        self.assertIn("error", result["result"])
        self.assertEqual(result["calls"], [])

    def test_calendar_page_uses_its_existing_token(self):
        result = self.probe(scripts='var TOK="fixture-token";')
        self.assertIn("data", result["result"])
        self.assertEqual(len(result["calls"]), 1)
        self.assertEqual(result["calls"][0]["action"], "hjaelperliste")
