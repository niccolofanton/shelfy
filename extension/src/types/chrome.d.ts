// Minimal ambient declarations for the chrome.* extension APIs the SPIKE-3 extension uses.
// Deliberately partial (no @types/chrome dependency): every member declared here is called
// somewhere in extension/src. Add members when new APIs are used, never speculatively.

declare namespace chrome {
  namespace runtime {
    const id: string | undefined;

    interface MessageSender {
      id?: string;
      url?: string;
      origin?: string;
      frameId?: number;
      documentId?: string;
      tab?: chrome.tabs.Tab;
    }

    interface Manifest {
      name: string;
      version: string;
      version_name?: string;
    }

    function getManifest(): Manifest;

    // MV3 promise form. Rejects when no listener exists (e.g. no side panel is open) and
    // throws "Extension context invalidated" in orphaned content scripts.
    function sendMessage<T = unknown>(message: unknown): Promise<T>;

    const onMessage: {
      addListener(
        callback: (
          message: unknown,
          sender: MessageSender,
          sendResponse: (response?: unknown) => void,
        ) => boolean | void,
      ): void;
    };

    const onInstalled: {
      addListener(callback: (details: { reason: string }) => void): void;
    };
  }

  namespace storage {
    interface StorageArea {
      get(keys?: string | string[] | null): Promise<Record<string, unknown>>;
      set(items: Record<string, unknown>): Promise<void>;
      clear(): Promise<void>;
    }

    const local: StorageArea;
  }

  namespace tabs {
    interface Tab {
      id?: number;
      windowId: number;
      active: boolean;
      // Present only with the "tabs" permission or a host permission for the tab's URL.
      url?: string;
    }

    function query(queryInfo: { active?: boolean; currentWindow?: boolean }): Promise<Tab[]>;

    // Delivers to the content scripts of the tab; rejects when none of this extension's
    // instance is listening (e.g. the tab was loaded before the extension was reloaded).
    function sendMessage<T = unknown>(tabId: number, message: unknown): Promise<T>;

    const onActivated: {
      addListener(callback: (activeInfo: { tabId: number; windowId: number }) => void): void;
    };

    const onUpdated: {
      addListener(
        callback: (tabId: number, changeInfo: { url?: string; status?: string }, tab: Tab) => void,
      ): void;
    };
  }

  namespace scripting {
    interface InjectionResult<T> {
      frameId: number;
      documentId?: string;
      result?: T;
    }

    // `func` is serialized with Function.prototype.toString() and evaluated in the target
    // world, so it must be self-contained. An async func resolves to its settled value.
    function executeScript<Args extends unknown[], Result>(injection: {
      target: { tabId: number; allFrames?: boolean; frameIds?: number[] };
      world?: 'ISOLATED' | 'MAIN';
      func: (...args: Args) => Result;
      args?: Args;
    }): Promise<Array<InjectionResult<Awaited<Result>>>>;
  }

  namespace sidePanel {
    function setPanelBehavior(behavior: { openPanelOnActionClick?: boolean }): Promise<void>;
  }
}
