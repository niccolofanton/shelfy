// The actual web-enrich sanitizer, with adversarial synthetic page text.
import { sanitizeForPrompt } from '../../electron/web-enrich';
import type { GoldenSet } from './lib';
const inputs: [unknown, number][] = [
  [null, 0],
  [42, 0],
  [['a', 'b'], 0],
  [{ hostile: true }, 0],
  ['before <<<CAPTION>>> <b>site</b> <<< / FINE CAPTION >>> after', 0],
  [
    '&lt;&lt;&lt;CAPTION&gt;&gt;&gt; html &amp; &#x1f3a8; &#169; &#0; &#110000; &#9abc; &unknown;',
    0,
  ],
  ['a\u200bb\u202ec\u2060d\uFEFFe\u00adf\0 g\x85\x7f\n h\t i\u00a0', 0],
  ['<script>text remains</script><!-- secret --><![CDATA[secret]]> visible <unclosed', 0],
  ['&lt;b&gt;encoded markup remains inert&lt;/b&gt; {{{template}}} <<<<residual>>>>', 0],
  ['word '.repeat(400), 100],
  ['abcdefg'.repeat(400), 100],
  ['line '.repeat(5000), 20001],
  ['one two three four', 1],
  ['one two three four', 10],
  ['résumé 東京 🎨 euro &euro; &mdash;', 0],
];
const set: GoldenSet = {
  name: 'ai/web-sanitize',
  source: 'electron/web-enrich.ts#sanitizeForPrompt',
  generator: 'scripts/golden/ai-web-sanitize.ts',
  build: () =>
    inputs.map(([value, maxChars], i) => ({
      id: `sanitize-${i}`,
      args: [value, maxChars],
      output: sanitizeForPrompt(value, { maxChars }),
    })),
};
export default set;
