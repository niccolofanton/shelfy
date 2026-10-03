// Minimal ambient declarations for the chrome.* extension APIs the extension uses. Deliberately
// partial (no @types/chrome dependency): every member declared here is called somewhere in
// extension/src. Add members when new APIs are used, never speculatively.

declare namespace chrome {
  namespace runtime {
    const id: string;

    interface MessageSender {
      id?: string;
      url?: string;
      origin?: string;
      frameId?: number;
      documentId?: string;
      tab?: chrome.tabs.Tab;
    }

    type MessageListener = (
      message: unknown,
      sender: MessageSender,
      sendResponse: (response?: unknown) => void,
    ) => boolean | void;

    // MV3 promise form. Rejects when no listener exists (e.g. no side panel is open) and
    // throws "Extension context invalidated" in orphaned content scripts.
    function sendMessage<T = unknown>(message: unknown): Promise<T>;

    const onMessage: { addListener(callback: MessageListener): void };
    /** Messages from web pages listed in externally_connectable (the Shelfy SPA). */
    const onMessageExternal: { addListener(callback: MessageListener): void };

    const onInstalled: {
      addListener(callback: (details: { reason: string }) => void): void;
    };

    /** Fires when a browser profile starts; registering it wakes the worker at start-up. */
    const onStartup: { addListener(callback: () => void): void };
  }

  namespace storage {
    type AccessLevel = 'TRUSTED_CONTEXTS' | 'TRUSTED_AND_UNTRUSTED_CONTEXTS';

    interface StorageArea {
      get(keys?: string | string[] | null): Promise<Record<string, unknown>>;
      set(items: Record<string, unknown>): Promise<void>;
      remove(keys: string | string[]): Promise<void>;
      /** Chrome 102+: who may use this area (content scripts are untrusted contexts). */
      setAccessLevel(options: { accessLevel: AccessLevel }): Promise<void>;
    }

    const local: StorageArea;
    /** In memory for the browser session; trusted contexts only by default. */
    const session: StorageArea;
  }

  namespace tabs {
    interface Tab {
      id?: number;
      windowId: number;
      active: boolean;
      status?: string;
      // Present only with the "tabs" permission or a host permission for the tab's URL.
      url?: string;
    }

    function update(tabId: number, properties: { url?: string; active?: boolean }): Promise<Tab>;
    function reload(tabId: number): Promise<void>;

    function query(queryInfo: { active?: boolean; currentWindow?: boolean }): Promise<Tab[]>;

    /** Rejects when no tab has this id (closed, or from before a browser restart). */
    function get(tabId: number): Promise<Tab>;

    // Delivers to the content scripts of the tab; rejects when none of this extension's
    // instance is listening (e.g. the tab was loaded before the extension was reloaded).
    // `frameId` or `documentId` (Chrome 106+) narrows it to one frame or document.
    function sendMessage<T = unknown>(
      tabId: number,
      message: unknown,
      options?: { frameId?: number; documentId?: string },
    ): Promise<T>;

    const onActivated: {
      addListener(callback: (activeInfo: { tabId: number; windowId: number }) => void): void;
    };

    const onUpdated: {
      addListener(
        callback: (tabId: number, changeInfo: { url?: string; status?: string }, tab: Tab) => void,
      ): void;
    };

    const onRemoved: {
      addListener(callback: (tabId: number, removeInfo: { windowId: number }) => void): void;
    };
  }

  namespace windows {
    interface Window {
      id?: number;
      type?: string;
      state?: string;
      tabs?: chrome.tabs.Tab[];
    }
    function create(options: {
      url: string;
      type: 'normal';
      state: 'normal' | 'minimized';
      focused: boolean;
    }): Promise<Window>;
    function get(id: number, options: { populate: boolean }): Promise<Window>;
    function update(
      id: number,
      options: { state: 'normal' | 'minimized'; focused: boolean },
    ): Promise<Window>;
  }
  namespace notifications {
    function create(
      id: string,
      options: {
        type: 'basic';
        iconUrl: string;
        title: string;
        message: string;
        buttons: { title: string }[];
      },
    ): Promise<string>;
    function clear(id: string): Promise<boolean>;
    const onClicked: { addListener(callback: (id: string) => void): void };
    const onButtonClicked: {
      addListener(callback: (id: string, buttonIndex: number) => void): void;
    };
  }

  namespace alarms {
    interface Alarm {
      name: string;
      scheduledTime: number;
      periodInMinutes?: number;
    }

    // Chrome 120+: `when` and delays shorter than 30 s fire after 30 s.
    function create(
      name: string,
      info: { when?: number; delayInMinutes?: number; periodInMinutes?: number },
    ): Promise<void>;

    function get(name: string): Promise<Alarm | undefined>;
    function clear(name: string): Promise<boolean>;

    const onAlarm: { addListener(callback: (alarm: Alarm) => void): void };
  }

  /** P2-13: the sync controller's MAIN-world helpers (main/replay.ts). */
  namespace scripting {
    type InjectionTarget =
      | { tabId: number; frameIds?: number[]; allFrames?: boolean }
      | { tabId: number; documentIds: string[] };

    interface InjectionResult {
      frameId: number;
      documentId?: string;
      result?: unknown;
    }

    // `func` is serialized with toString(): it must not reference anything outside itself.
    // `args` are JSON-serializable values passed to it.
    function executeScript(injection: {
      target: InjectionTarget;
      world?: 'ISOLATED' | 'MAIN';
      func: (...args: never[]) => unknown;
      args?: unknown[];
      injectImmediately?: boolean;
    }): Promise<InjectionResult[]>;
  }

  namespace sidePanel {
    function setPanelBehavior(behavior: { openPanelOnActionClick?: boolean }): Promise<void>;
  }
}
