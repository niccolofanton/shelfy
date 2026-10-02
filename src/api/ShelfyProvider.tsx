import React, { createContext, useContext } from 'react';
import { getElectronClient } from './electronClient';
import type { ShelfyCapabilities, ShelfyClient } from './ShelfyClient';

const ShelfyContext = createContext<ShelfyClient | null>(null);

// Hands the app's ShelfyClient to every view: the desktop entry passes the
// electron client, the web entry the HTTP one.
export function ShelfyProvider({
  client,
  children,
}: {
  client: ShelfyClient;
  children: React.ReactNode;
}): React.JSX.Element {
  return <ShelfyContext.Provider value={client}>{children}</ShelfyContext.Provider>;
}

// The client of the nearest ShelfyProvider. Without one, the desktop client:
// component tests render views in isolation against the mocked bridge.
export function useShelfy(): ShelfyClient {
  return useContext(ShelfyContext) ?? getElectronClient();
}

export function useCapabilities(): ShelfyCapabilities {
  return useShelfy().capabilities;
}
