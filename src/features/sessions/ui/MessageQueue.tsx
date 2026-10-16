import { useEffect, useRef, useState } from "react";
import {
  Bell,
  Check,
  CornerDownRight,
  ListEnd,
  Pause,
  Pencil,
  Play,
  Trash2,
  X,
} from "../../../shared/ui/icons";
import { isImeComposition } from "../../../shared/lib/keyboard";
import {
  parseMonoNotice,
  type MonoNotice,
} from "../../monos/model/monoMessages";
import type { MessageQueueStatus, QueuedMessage } from "../model/session";

export function MessageQueue({
  messages,
  status,
  pauseNotice,
  onDelete,
  onEdit,
  onEditingChange,
  onSteer,
  onResume,
  variant = "queue",
  sendingId,
}: {
  messages: QueuedMessage[];
  status?: MessageQueueStatus;
  /** Why the queue paused, when the user did not pause it. */
  pauseNotice?: string;
  onDelete?: (messageId: string) => void;
  onEdit?: (messageId: string, text: string) => void;
  onEditingChange?: (messageId?: string) => void;
  onSteer?: (messageId: string) => void;
  onResume?: () => void;
  variant?: "queue" | "messages";
  sendingId?: string;
}) {
  const [editingId, setEditingId] = useState<string>();
  const [editDraft, setEditDraft] = useState("");
  const onEditingChangeRef = useRef(onEditingChange);
  onEditingChangeRef.current = onEditingChange;
  const editingIdRef = useRef(editingId);
  editingIdRef.current = editingId;
  useEffect(() => {
    return () => {
      if (editingIdRef.current) onEditingChangeRef.current?.();
    };
  }, []);
  if (messages.length === 0) return null;
  const paused = status === "paused";
  const failed = messages.some((message) => message.error);
  const notices = messages.flatMap((message) => {
    const notice = parseMonoNotice(message);
    return notice ? [{ id: message.id, notice }] : [];
  });
  const rows = messages.filter(
    (message) => !notices.some((entry) => entry.id === message.id),
  );
  const statusLine = variant === "messages" && !paused && rows.length > 0;

  const startEdit = (message: QueuedMessage) => {
    setEditingId(message.id);
    setEditDraft(message.text);
    onEditingChange?.(message.id);
  };
  const cancelEdit = () => {
    setEditingId(undefined);
    setEditDraft("");
    onEditingChange?.();
  };
  const saveEdit = (message: QueuedMessage) => {
    if (!editDraft.trim() && message.attachments.length === 0) return;
    onEdit?.(message.id, editDraft);
    setEditingId(undefined);
    setEditDraft("");
  };

  return (
    <div className="px-2 text-content/55" data-message-queue>
      <div
        className="relative z-0 rounded-t-[10px] border border-b-0 border-content/10 bg-content/3 px-2 py-1"
        data-message-queue-card
      >
        {paused ? (
          <div className="flex h-7 items-center gap-2 border-b border-stroke text-[12px]">
            <Pause className="size-3.5" />
            <span className="min-w-0 flex-1 truncate">
              {failed
                ? "A message couldn't be sent"
                : (pauseNotice ??
                  (variant === "messages"
                    ? "Messages paused"
                    : "Queue paused because you interrupted"))}
            </span>
            <button
              type="button"
              onClick={onResume}
              className="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 hover:bg-content/10 hover:text-content"
            >
              <Play className="size-3.5" />
              {failed ? "Retry" : "Resume"}
            </button>
          </div>
        ) : null}
        {notices.length > 0 ? (
          <>
            <div className="py-1 text-[11px] text-content/50">
              Notice from the app · opens your next turn
            </div>
            <div className="divide-y divide-stroke">
              {notices.map(({ id, notice }) => (
                <NoticeRow
                  key={id}
                  notice={notice}
                  disabled={sendingId === id}
                  onRemove={() => onDelete?.(id)}
                />
              ))}
            </div>
          </>
        ) : null}
        {statusLine ? (
          <div
            role="status"
            className={`py-1 text-[11px] text-content/50 ${
              notices.length > 0 ? "border-t border-stroke" : ""
            }`}
          >
            {sendingId ? "Sending…" : "Waiting to send"}
          </div>
        ) : null}
        {rows.map((message, index) => {
          const sending = sendingId === message.id;
          const editing = editingId === message.id;
          const label =
            (message.monoSessionCompletion
              ? message.monoSessionCompletion.sessionCount
                ? `${message.monoSessionCompletion.sessionCount} sessions finished`
                : `Session ${message.monoSessionCompletion.status}: ${message.monoSessionCompletion.title}`
              : message.text.trim()) ||
            message.noteCard?.title ||
            message.handoffCard?.brief ||
            `${message.attachments.length} attachment${message.attachments.length === 1 ? "" : "s"}`;
          return (
            <div
              key={message.id}
              className={`flex min-h-7 items-center gap-2 text-[12px] ${
                index > 0 || (notices.length > 0 && !statusLine)
                  ? "border-t border-stroke"
                  : ""
              }`}
            >
              <ListEnd className="size-3.5 shrink-0" />
              {editing ? (
                <>
                  <textarea
                    autoFocus
                    aria-label="Edit queued message"
                    value={editDraft}
                    rows={1}
                    onChange={(event) => setEditDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (isImeComposition(event.nativeEvent)) return;
                      if (event.key === "Escape") {
                        event.preventDefault();
                        cancelEdit();
                      } else if (event.key === "Enter" && !event.shiftKey) {
                        event.preventDefault();
                        saveEdit(message);
                      }
                    }}
                    className="min-h-6 min-w-0 flex-1 resize-none rounded-md border border-content/15 bg-content/5 px-1.5 py-0.5 text-[12px] text-content outline-none focus:border-content/30"
                  />
                  <button
                    type="button"
                    title="Save queued message"
                    aria-label="Save queued message"
                    disabled={
                      !editDraft.trim() && message.attachments.length === 0
                    }
                    onClick={() => saveEdit(message)}
                    className="grid size-6 shrink-0 place-items-center rounded-md hover:bg-content/10 hover:text-content disabled:opacity-30"
                  >
                    <Check className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    title="Cancel queued message edit"
                    aria-label="Cancel queued message edit"
                    onClick={cancelEdit}
                    className="grid size-6 shrink-0 place-items-center rounded-md hover:bg-content/10 hover:text-content"
                  >
                    <X className="size-3.5" />
                  </button>
                </>
              ) : (
                <>
                  <div className="min-w-0 flex-1 py-1">
                    <div className="truncate text-content/80" title={label}>
                      {label}
                    </div>
                    {message.error ? (
                      <p role="alert" className="text-red-400">
                        {message.error}
                      </p>
                    ) : null}
                  </div>
                  {onSteer && !message.monoSessionCompletion ? (
                    <button
                      type="button"
                      disabled={sending}
                      onClick={() => onSteer?.(message.id)}
                      className="flex h-6 shrink-0 items-center gap-1.5 rounded-md px-1.5 hover:bg-content/10 hover:text-content"
                    >
                      <CornerDownRight className="size-3.5" />
                      Steer
                    </button>
                  ) : null}
                  <button
                    type="button"
                    title="Edit queued message"
                    aria-label="Edit queued message"
                    disabled={sending || !!message.monoSessionCompletion}
                    onClick={() => startEdit(message)}
                    className="grid size-6 shrink-0 place-items-center rounded-md hover:bg-content/10 hover:text-content"
                  >
                    <Pencil className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    title="Remove queued message"
                    aria-label="Remove queued message"
                    disabled={sending}
                    onClick={() => onDelete?.(message.id)}
                    className="grid size-6 shrink-0 place-items-center rounded-md hover:bg-content/10 hover:text-content"
                  >
                    <Trash2 className="size-3.5" />
                  </button>
                </>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const NOTICE_SENTENCE: Record<
  MonoNotice["status"],
  { label: string; text: (recipient: string) => string }
> = {
  unanswered: {
    label: "Unanswered",
    text: (recipient) => `${recipient} did not reply to`,
  },
  dropped: { label: "Dropped", text: (recipient) => `${recipient} never got` },
};

/** Design U10: a sender notice, in words, with only a remove control. */
function NoticeRow({
  notice,
  disabled,
  onRemove,
}: {
  notice: MonoNotice;
  disabled: boolean;
  onRemove: () => void;
}) {
  const sentence = NOTICE_SENTENCE[notice.status];
  // renderMonoNotice keeps 60 characters, so a full line was cut.
  const quote = `“${notice.line}${Array.from(notice.line).length >= 60 ? "…" : ""}”`;
  const text = `${sentence.text(notice.recipient)} ${quote}`;
  return (
    <div className="flex min-h-7 items-center gap-2 text-[12px]">
      <Bell className="size-3.5 shrink-0 text-amber-400" />
      <div
        className="min-w-0 flex-1 truncate py-1 text-content/80"
        title={`${sentence.label} · ${text}`}
      >
        <span className="text-amber-400">{sentence.label}</span> · {text}
      </div>
      <button
        type="button"
        title="Remove notice"
        aria-label="Remove notice"
        disabled={disabled}
        onClick={onRemove}
        className="grid size-6 shrink-0 place-items-center rounded-md hover:bg-content/10 hover:text-content"
      >
        <X className="size-3.5" />
      </button>
    </div>
  );
}
