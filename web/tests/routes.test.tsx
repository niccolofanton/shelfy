// The web app's route table (web/src/routes.tsx) and its Navigation on the
// address bar.
import { describe, it, expect, vi, afterEach } from 'vitest';
import React from 'react';
import { render, screen, act, waitFor } from '@testing-library/react';
import { useNavigation, type Navigation } from '@ui/api/navigation';
import {
  WebNavigation,
  loginPath,
  parseRoute,
  pathOf,
  patternOf,
  safeNext,
  takeMagicToken,
  type WebRoute,
} from '../src/routes';

afterEach(() => {
  window.history.replaceState(null, '', '/');
});

describe('route table', () => {
  it('reads every route and writes it back', () => {
    const cases: [string, WebRoute][] = [
      ['/', { name: 'library' }],
      ['/c/12', { name: 'collection', collectionId: 12 }],
      ['/p/ig_3141', { name: 'post', key: 'ig_3141' }],
      ['/trash', { name: 'trash' }],
      ['/settings/storage', { name: 'settings', section: 'storage' }],
      ['/device', { name: 'device' }],
    ];
    for (const [path, route] of cases) {
      expect(parseRoute(path), path).toEqual(route);
      expect(pathOf(route as Parameters<typeof pathOf>[0]), path).toBe(path);
    }
  });

  it('reads the sign-in pages', () => {
    expect(parseRoute('/login', '?error=invalid_link&next=%2Fc%2F4')).toEqual({
      name: 'login',
      error: 'invalid_link',
      next: '/c/4',
    });
    expect(parseRoute('/login')).toEqual({ name: 'login', error: null, next: null });
    expect(parseRoute('/login/magic')).toEqual({ name: 'magic' });
  });

  it('opens the first Settings section and tolerates a trailing slash', () => {
    expect(parseRoute('/settings')).toEqual({ name: 'settings', section: 'account' });
    expect(parseRoute('/trash/')).toEqual({ name: 'trash' });
    expect(parseRoute('/c/7/')).toEqual({ name: 'collection', collectionId: 7 });
  });

  it('has nothing at other addresses or with invalid parameters', () => {
    for (const path of [
      '/nope',
      '/c/abc',
      '/c/0',
      '/c/-3',
      '/c/1/2',
      '/p',
      '/settings/Bad%20Section',
      '/trash/old',
      '/login/elsewhere',
    ]) {
      expect(parseRoute(path), path).toEqual({ name: 'notFound' });
    }
  });

  it('names each route by its pattern, never by its address', () => {
    expect(patternOf(parseRoute('/p/ig_1'))).toBe('/p/:key');
    expect(patternOf(parseRoute('/c/5'))).toBe('/c/:collectionId');
    expect(patternOf(parseRoute('/settings/legal'))).toBe('/settings/:section');
    expect(patternOf(parseRoute('/'))).toBe('/');
    expect(patternOf(parseRoute('/login', '?next=/c/5'))).toBe('/login');
    expect(patternOf(parseRoute('/somewhere/else'))).toBe('/*');
  });
});

describe('after sign-in', () => {
  it('returns only to a page of the app, rebuilt from its route', () => {
    expect(safeNext('/c/3')).toBe('/c/3');
    expect(safeNext('/p/ig_1?tab=ai#top')).toBe('/p/ig_1');
    expect(safeNext('/settings')).toBe('/settings/account');
    expect(safeNext('/device')).toBe('/device');
    for (const next of [
      null,
      '',
      'c/3',
      '//evil.example',
      '/\\evil.example',
      'https://evil.example/c/1',
      '/login',
      '/login/magic',
      '/nowhere',
    ]) {
      expect(safeNext(next), String(next)).toBeNull();
    }
  });

  it('remembers the page asked for on the sign-in page', () => {
    expect(loginPath('/c/3')).toBe('/login?next=%2Fc%2F3');
    expect(loginPath('/')).toBe('/login');
    expect(loginPath('/nowhere')).toBe('/login');
    expect(loginPath(null)).toBe('/login');
  });
});

describe('sign-in link token', () => {
  it('takes the token out of the address at once', () => {
    const history = { replaceState: vi.fn() };
    expect(takeMagicToken({ pathname: '/login/magic', hash: '#tok_123' }, history)).toBe('tok_123');
    expect(history.replaceState).toHaveBeenCalledWith(null, '', '/login/magic');
  });

  it('finds none elsewhere or without a token', () => {
    const history = { replaceState: vi.fn() };
    expect(takeMagicToken({ pathname: '/', hash: '#tok' }, history)).toBeNull();
    expect(takeMagicToken({ pathname: '/login/magic/', hash: '' }, history)).toBeNull();
    expect(history.replaceState).not.toHaveBeenCalled();
    expect(takeMagicToken({ pathname: '/login/magic', hash: '#' }, history)).toBeNull();
    expect(history.replaceState).toHaveBeenCalledTimes(1);
  });
});

describe('WebNavigation', () => {
  let nav: Navigation | null = null;
  function Probe(): React.JSX.Element {
    nav = useNavigation();
    return <span data-testid="route">{JSON.stringify(nav?.route)}</span>;
  }
  function renderAt(path: string) {
    window.history.replaceState(null, '', path);
    render(
      <WebNavigation>
        <Probe />
      </WebNavigation>,
    );
  }
  const route = () => JSON.parse(screen.getByTestId('route').textContent ?? 'null');

  it('follows the address', () => {
    renderAt('/c/3');
    expect(route()).toEqual({ name: 'collection', collectionId: 3 });
    act(() => window.history.pushState(null, '', '/trash'));
    expect(route()).toEqual({ name: 'trash' });
  });

  it('pushes a route, and goes nowhere for the one on screen', () => {
    renderAt('/');
    const length = window.history.length;
    act(() => nav!.navigate({ name: 'collection', collectionId: 8 }));
    expect(window.location.pathname).toBe('/c/8');
    expect(window.history.length).toBe(length + 1);
    expect(route()).toEqual({ name: 'collection', collectionId: 8 });
    act(() => nav!.navigate({ name: 'collection', collectionId: 8 }));
    expect(window.history.length).toBe(length + 1);
    act(() => nav!.navigate({ name: 'trash' }, { replace: true }));
    expect(window.location.pathname).toBe('/trash');
    expect(window.history.length).toBe(length + 1);
  });

  it('goes back to the page the app came from', async () => {
    renderAt('/c/3');
    act(() => nav!.navigate({ name: 'post', key: 'ig_1' }));
    expect(window.location.pathname).toBe('/p/ig_1');
    act(() => nav!.back({ name: 'library' }));
    await waitFor(() => expect(window.location.pathname).toBe('/c/3'));
    expect(route()).toEqual({ name: 'collection', collectionId: 3 });
  });

  it('leaves a deep link for the fallback, in place', () => {
    renderAt('/p/ig_1');
    const length = window.history.length;
    act(() => nav!.back({ name: 'library' }));
    expect(window.location.pathname).toBe('/');
    expect(window.history.length).toBe(length);
    expect(route()).toEqual({ name: 'library' });
  });

  it('keeps navigate and back across route changes', () => {
    renderAt('/');
    const { navigate, back } = nav!;
    act(() => nav!.navigate({ name: 'trash' }));
    expect(nav!.navigate).toBe(navigate);
    expect(nav!.back).toBe(back);
  });
});
