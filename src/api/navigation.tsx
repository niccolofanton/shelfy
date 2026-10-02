import React, { createContext, useContext } from 'react';

// The app's addresses (web port plan §2.19 Routes), in the UI's terms. The web
// app keeps them in the address bar: web/src/routes.tsx maps them to and from
// URLs and provides the Navigation. The desktop has no addresses and keeps its
// view-state navigation: it provides none, and useNavigation() is null there.
//
// | Route        | Web address          | App shows                           |
// |--------------|----------------------|-------------------------------------|
// | `library`    | `/`                  | the gallery, every post             |
// | `collection` | `/c/:collectionId`   | the gallery, one folder             |
// | `post`       | `/p/:key`            | the post's modal, over the gallery  |
// | `trash`      | `/trash`             | the trash                           |
// | `settings`   | `/settings/:section` | Settings, at one section            |
export type AppRoute =
  | { name: 'library' }
  | { name: 'collection'; collectionId: number }
  | { name: 'post'; key: string }
  | { name: 'trash' }
  | { name: 'settings'; section: string };

// What the address says: a route, or nothing the app knows.
export type CurrentRoute = AppRoute | { name: 'notFound' };

// The section `/settings` opens.
export const DEFAULT_SETTINGS_SECTION = 'account';

export interface NavigateOptions {
  // Replace the current history entry instead of adding one.
  replace?: boolean;
}

export interface Navigation {
  readonly route: CurrentRoute;
  // Goes to `to`. Going where the address already is does nothing.
  navigate(to: AppRoute, options?: NavigateOptions): void;
  // Leaves the current route, as closing a post's modal does: back to the
  // previous entry when it is one of the app's, else to `fallback` in place
  // (a deep link has nothing in the app to go back to).
  back(fallback: AppRoute): void;
}

const NavigationContext = createContext<Navigation | null>(null);

export function NavigationProvider({
  navigation,
  children,
}: {
  navigation: Navigation;
  children: React.ReactNode;
}): React.JSX.Element {
  return <NavigationContext.Provider value={navigation}>{children}</NavigationContext.Provider>;
}

// The address bar's Navigation, or null on the desktop.
export function useNavigation(): Navigation | null {
  return useContext(NavigationContext);
}

export function sameRoute(a: CurrentRoute, b: CurrentRoute): boolean {
  switch (a.name) {
    case 'collection':
      return b.name === 'collection' && b.collectionId === a.collectionId;
    case 'post':
      return b.name === 'post' && b.key === a.key;
    case 'settings':
      return b.name === 'settings' && b.section === a.section;
    default:
      return a.name === b.name;
  }
}
