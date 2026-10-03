// The template language of the prompt files in shared/ai/ (`*.md`). The Rust core renders
// the same files with its own copy of these rules (crates/core/src/ai/template.rs), and the
// golden fixtures under shared/golden/ai/ keep the two identical.
//
// A template is the file's text without its final line break, read line by line:
//
//   {{#if name}} … {{else}} … {{/if}}
//   {{#unless name}} … {{else}} … {{/unless}}
//       Each on a line of its own. The lines between are kept when `name` is true or a
//       non-empty text (`#unless`: when it is not). Sections nest; `{{else}}` is optional.
//   {{! a comment }}
//       A line of its own, never output.
//   {{name}}
//       Anywhere in any other line: the text of the variable `name`, inserted as it is. A
//       value is never read again as template syntax, so untrusted text cannot inject any.
//
// The kept lines are joined with "\n". It is an error when the template uses a variable the
// caller did not give (in any branch), when a section is unbalanced, or when a directive is
// not alone on its line: a typo in a prompt fails the tests instead of reaching a model.

/** The variables of a render: texts, or flags for sections. */
export type TemplateVars = Readonly<Record<string, string | boolean>>;

/** A template that cannot be rendered with the given variables. */
export class TemplateError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'TemplateError';
  }
}

const NAME = '[A-Za-z][A-Za-z0-9_]*';
const OPEN = new RegExp(`^\\{\\{\\s*#(if|unless)\\s+(${NAME})\\s*\\}\\}$`);
const ELSE = /^\{\{\s*else\s*\}\}$/;
const CLOSE = /^\{\{\s*\/(if|unless)\s*\}\}$/;
const COMMENT = /^\{\{!.*\}\}$/;
const VARIABLE = new RegExp(`\\{\\{\\s*(${NAME})\\s*\\}\\}`, 'g');
const STRAY = /\{\{\s*(?:[#/!]|else\s*\}\})/;

interface Section {
  kind: 'if' | 'unless';
  /** Whether the enclosing lines are output. */
  outer: boolean;
  /** Whether the section's condition holds (for `unless`: does not hold). */
  holds: boolean;
  inElse: boolean;
  line: number;
}

/** Strips spaces and tabs at both ends, the only blanks a directive line may carry. */
function trimBlanks(line: string): string {
  return line.replace(/^[ \t]+|[ \t]+$/g, '');
}

function lookup(vars: TemplateVars, name: string, line: number): string | boolean {
  if (!Object.prototype.hasOwnProperty.call(vars, name)) {
    throw new TemplateError(`line ${line}: unknown variable "${name}"`);
  }
  return vars[name];
}

/** Renders `template` (a prompt file's text) with `vars`. */
export function renderTemplate(template: string, vars: TemplateVars): string {
  const text = template.endsWith('\n') ? template.slice(0, -1) : template;
  const out: string[] = [];
  const stack: Section[] = [];
  let on = true;
  const lines = text.split('\n');
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const n = i + 1;
    const bare = trimBlanks(line);
    const open = OPEN.exec(bare);
    if (open) {
      const value = lookup(vars, open[2], n);
      const truthy = value === true || (typeof value === 'string' && value !== '');
      const kind = open[1] as 'if' | 'unless';
      const holds = kind === 'if' ? truthy : !truthy;
      stack.push({ kind, outer: on, holds, inElse: false, line: n });
      on = on && holds;
      continue;
    }
    if (ELSE.test(bare)) {
      const top = stack[stack.length - 1];
      if (!top || top.inElse) {
        throw new TemplateError(`line ${n}: {{else}} outside a section, or a second one`);
      }
      top.inElse = true;
      on = top.outer && !top.holds;
      continue;
    }
    const close = CLOSE.exec(bare);
    if (close) {
      const top = stack.pop();
      if (!top) throw new TemplateError(`line ${n}: {{/${close[1]}}} closes no section`);
      if (top.kind !== close[1]) {
        throw new TemplateError(
          `line ${n}: {{/${close[1]}}} closes {{#${top.kind}}} of line ${top.line}`,
        );
      }
      on = top.outer;
      continue;
    }
    if (COMMENT.test(bare)) continue;
    if (STRAY.test(line)) {
      throw new TemplateError(`line ${n}: a directive must be alone on its line`);
    }
    // Every variable must exist, in every branch; only the kept lines are filled in.
    const rendered = line.replace(VARIABLE, (_match, name: string) => {
      const value = lookup(vars, name, n);
      if (typeof value !== 'string') {
        throw new TemplateError(`line ${n}: "${name}" is a flag, not a text`);
      }
      return value;
    });
    if (on) out.push(rendered);
  }
  const unclosed = stack.pop();
  if (unclosed)
    throw new TemplateError(`line ${unclosed.line}: {{#${unclosed.kind}}} is not closed`);
  return out.join('\n');
}
