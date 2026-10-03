// Small DOM helpers for the side panel. Every string coming from the worker or a page is set as
// text, never as HTML.

export function byId<T extends HTMLElement>(id: string): T {
  const node = document.getElementById(id);
  if (!node) throw new Error(`panel.html is missing #${id}`);
  return node as T;
}

export function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  options: { text?: string | number; className?: string; attrs?: Record<string, string> } = {},
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (options.text !== undefined) node.textContent = String(options.text);
  if (options.className) node.className = options.className;
  for (const [name, value] of Object.entries(options.attrs ?? {})) node.setAttribute(name, value);
  return node;
}

export function button(
  text: string,
  onClick: () => void,
  options: { className?: string; testId?: string } = {},
): HTMLButtonElement {
  const node = el('button', {
    text,
    className: options.className,
    attrs: { type: 'button', ...(options.testId ? { 'data-testid': options.testId } : {}) },
  });
  node.addEventListener('click', onClick);
  return node;
}

export function setText(node: HTMLElement, text: string, className?: string): void {
  node.textContent = text;
  if (className !== undefined) node.className = className;
}

/** A button that asks for a second click within `ms` before acting. */
export function confirmButton(
  text: string,
  confirmText: string,
  onConfirm: () => void,
  options: { className?: string; testId?: string; ms?: number } = {},
): HTMLButtonElement {
  let armedUntil = 0;
  const node = button(
    text,
    () => {
      if (Date.now() < armedUntil) {
        armedUntil = 0;
        node.textContent = text;
        onConfirm();
        return;
      }
      armedUntil = Date.now() + (options.ms ?? 4000);
      node.textContent = confirmText;
      setTimeout(() => {
        if (Date.now() >= armedUntil) node.textContent = text;
      }, options.ms ?? 4000);
    },
    options,
  );
  return node;
}
