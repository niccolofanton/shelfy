// Persisted activity belongs to the signed-in account. Job controls reuse JobsApi
// (EXECUTION L14); no second job transport or desktop IPC is introduced here.
export interface ActivityNotification {
  id: number;
  kind: string;
  code: string;
  params: Record<string, unknown>;
  target: string | null;
  createdAt: number;
  readAt: number | null;
}
export interface NotificationPage {
  items: ActivityNotification[];
  nextCursor: string | null;
  unreadCount: number;
}
export type NotificationReadSelector =
  | { ids: number[]; upTo?: never }
  | { upTo: number; ids?: never };
export interface ActivityApi {
  list(page: { limit: number; cursor?: string | null }): Promise<NotificationPage>;
  read(selector: NotificationReadSelector): Promise<{ updated: number; unreadCount: number }>;
  onNotification(listener: (notification: ActivityNotification) => void): () => void;
  // Refresh after reconnect as well as a replay gap: read markers may have
  // changed on another device without a new notification event.
  onRefresh(listener: () => void): () => void;
}
