// The stack of open overlays (dialogs, sheets, popovers). Only the topmost
// handles Escape and traps Tab, so Escape closes a menu before the dialog it
// was opened from. useDialog and Popover push while open and pop on close.
const stack: symbol[] = [];

export function pushLayer(name = 'layer'): symbol {
  const token = Symbol(name);
  stack.push(token);
  return token;
}

export function popLayer(token: symbol): void {
  const i = stack.lastIndexOf(token);
  if (i >= 0) stack.splice(i, 1);
}

export function isTopLayer(token: symbol): boolean {
  return stack[stack.length - 1] === token;
}
