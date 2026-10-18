-- A monocode.db from before Change M: the schema of MonoCode 0.13.0 (no
-- milestones tables), with two Monos and one older session. run.sh replaces
-- @HOME@, @A@ and @B@ with the eval HOME and the two project paths.
-- Beta sorts first, so its held habit run starts before Alpha's.
CREATE TABLE schema_migrations (
           version INTEGER PRIMARY KEY,
           applied_at INTEGER NOT NULL
         );
CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  cwd TEXT NOT NULL,
  harness TEXT NOT NULL,
  model TEXT NOT NULL,
  model_settings TEXT NOT NULL DEFAULT '{}',
  runtime_mode TEXT NOT NULL,
  title TEXT NOT NULL,
  provider_session_id TEXT,
  blocks_json TEXT NOT NULL DEFAULT '[]',
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
, branch TEXT, context_used INTEGER, context_window INTEGER, archived INTEGER NOT NULL DEFAULT 0, worktree_cwd TEXT, has_user_message INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0, linked_work_item_json TEXT, provider_account_id TEXT, worktree_removed INTEGER NOT NULL DEFAULT 0, is_draft INTEGER NOT NULL DEFAULT 0, automation_id TEXT, queued_messages_json TEXT NOT NULL DEFAULT '[]', queue_status TEXT, sidebar_hidden INTEGER NOT NULL DEFAULT 0, inbox_ask TEXT);
CREATE TABLE in_flight_sessions (
               session_id TEXT PRIMARY KEY,
               cwd TEXT NOT NULL,
               sort_index INTEGER NOT NULL
             );
CREATE TABLE workspace_snapshot (
               id INTEGER PRIMARY KEY CHECK (id = 1),
               snapshot_json TEXT NOT NULL,
               updated_at INTEGER NOT NULL
             );
CREATE TABLE notes (
           id TEXT PRIMARY KEY,
           slug TEXT NOT NULL UNIQUE,
           title TEXT NOT NULL,
           body TEXT NOT NULL DEFAULT '',
           tags_json TEXT NOT NULL DEFAULT '[]',
           source_session_id TEXT,
           source_cwd TEXT,
           slug_pending INTEGER NOT NULL DEFAULT 0,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         , content_kind TEXT NOT NULL DEFAULT 'note', artifact_kind TEXT);
CREATE INDEX notes_updated_idx
           ON notes (updated_at DESC, id);
CREATE INDEX sessions_cwd_cover_idx
               ON sessions (cwd, has_user_message, updated_at DESC, id, harness,
                            model, runtime_mode, title, provider_session_id,
                            created_at, branch, archived, pinned,
                            linked_work_item_json, worktree_cwd, worktree_removed,
                            is_draft, automation_id, sidebar_hidden);
CREATE TABLE worktree_removals (
           path TEXT PRIMARY KEY,
           sessions_json TEXT NOT NULL
         );
CREATE INDEX sessions_legacy_inbox
         ON sessions (id) WHERE inbox_ask IS NOT NULL;
CREATE TABLE session_reminders (
           session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
           due_at INTEGER NOT NULL CHECK (due_at > 0),
           fired_at INTEGER
         );
CREATE INDEX session_reminders_pending
         ON session_reminders (due_at) WHERE fired_at IS NULL;
CREATE TABLE automations (
           id TEXT PRIMARY KEY,
           definition_json TEXT NOT NULL,
           enabled INTEGER NOT NULL,
           next_run_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
CREATE INDEX automations_due_idx
           ON automations (enabled, next_run_at);
CREATE TABLE automation_runs (
           id TEXT PRIMARY KEY,
           automation_id TEXT NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
           created_at INTEGER NOT NULL,
           run_json TEXT NOT NULL
         );
CREATE INDEX automation_runs_history_idx
           ON automation_runs (automation_id, created_at DESC);
CREATE TABLE automation_event_claims (
           automation_id TEXT NOT NULL REFERENCES automations(id) ON DELETE CASCADE,
           event_key TEXT NOT NULL,
           created_at INTEGER NOT NULL,
           PRIMARY KEY (automation_id, event_key)
         );
CREATE TABLE orchestration_runs (lead_id TEXT PRIMARY KEY, state TEXT NOT NULL);
CREATE TABLE orchestration_sidebar (lead_id TEXT PRIMARY KEY, summary TEXT NOT NULL);
CREATE TABLE orchestration_workers (session_id TEXT PRIMARY KEY, lead_id TEXT NOT NULL);
CREATE TABLE mono_transcripts (
           session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE
         );
CREATE TABLE mono_turns (
           session_id TEXT NOT NULL REFERENCES mono_transcripts(session_id) ON DELETE CASCADE,
           seq INTEGER NOT NULL,
           PRIMARY KEY (session_id, seq)
         );
CREATE TABLE mono_blocks (
           session_id TEXT NOT NULL REFERENCES mono_transcripts(session_id) ON DELETE CASCADE,
           seq INTEGER NOT NULL,
           block_id TEXT NOT NULL,
           block_json TEXT NOT NULL,
           search_text TEXT NOT NULL,
           is_user INTEGER NOT NULL,
           is_draft INTEGER NOT NULL,
           PRIMARY KEY (session_id, seq),
           UNIQUE (session_id, block_id)
         );
CREATE INDEX mono_blocks_user ON mono_blocks(session_id, is_user, is_draft);
CREATE TABLE monos (
           id TEXT PRIMARY KEY
             CHECK (length(id) BETWEEN 1 AND 64 AND id NOT GLOB '*[^A-Za-z0-9-]*'),
           name TEXT NOT NULL,
           role TEXT NOT NULL CHECK (role IN ('giga', 'mono')),
           data_json TEXT NOT NULL,
           sort_order INTEGER NOT NULL,
           created_at INTEGER NOT NULL,
           updated_at INTEGER NOT NULL
         );
CREATE TABLE monos_legacy_import (
           id INTEGER PRIMARY KEY CHECK (id = 1),
           imported_at INTEGER NOT NULL
         );
CREATE TABLE mono_messages (
           id                 TEXT PRIMARY KEY CHECK (length(id) = 14 AND substr(id, 1, 2) = 'm_'),
           request_id         TEXT NOT NULL CHECK (length(request_id) BETWEEN 1 AND 128),
           part               INTEGER NOT NULL DEFAULT 0 CHECK (part BETWEEN 0 AND 9),
           payload_hash       TEXT NOT NULL CHECK (length(payload_hash) = 64),
           tool               TEXT NOT NULL CHECK (tool IN ('send_message', 'reply', 'start_session', 'app_action')),
           from_mono_id       TEXT REFERENCES monos(id) ON DELETE SET NULL,
           from_session_id    TEXT NOT NULL,
           to_mono_id         TEXT REFERENCES monos(id) ON DELETE SET NULL,
           to_session_id      TEXT,
           addressed          TEXT NOT NULL,
           body               TEXT NOT NULL CHECK (length(body) <= 8000),
           body_chars         INTEGER NOT NULL CHECK (body_chars >= 0),
           wanting_reply      INTEGER NOT NULL CHECK (wanting_reply IN (0, 1)),
           intent             TEXT NOT NULL CHECK (intent IN ('new', 'reply', 'steer', 'continue')),
           reply_to           TEXT REFERENCES mono_messages(id),
           scope              TEXT CHECK (scope IN ('turn')),
           scope_turn_id      TEXT CHECK (scope_turn_id IS NULL OR length(scope_turn_id) BETWEEN 1 AND 128),
           hop                INTEGER NOT NULL CHECK (hop >= 0),
           status             TEXT NOT NULL CHECK (status IN
                                ('open', 'answered', 'delivered', 'unanswered', 'dropped', 'rejected')),
           gate_result        TEXT NOT NULL,
           handled_by_session TEXT,
           delivery           TEXT CHECK (delivery IN ('submit', 'queue', 'steer')),
           turn_id            TEXT,
           slot_window        TEXT,
           handle             INTEGER CHECK (handle BETWEEN 1 AND 99),
           delivered_at       INTEGER,
           closed_at          INTEGER,
           notified_at        INTEGER,
           created_at         INTEGER NOT NULL,
           updated_at         INTEGER NOT NULL,
           UNIQUE (from_session_id, request_id, part),
           CHECK ((status = 'rejected') = (gate_result <> 'ok')),
           CHECK (status NOT IN ('open', 'unanswered') OR wanting_reply = 1),
           CHECK (status <> 'delivered' OR wanting_reply = 0),
           CHECK (handle IS NULL OR (turn_id IS NOT NULL AND wanting_reply = 1)),
           CHECK (status <> 'rejected' OR (handled_by_session IS NULL AND delivery IS NULL AND turn_id IS NULL)),
           CHECK (to_mono_id IS NULL OR to_session_id IS NULL),
           CHECK (status <> 'answered' OR wanting_reply = 1),
           CHECK (status IN ('rejected', 'dropped') OR (handled_by_session IS NOT NULL AND delivery IS NOT NULL)),
           CHECK (delivered_at IS NULL OR turn_id IS NOT NULL),
           CHECK (turn_id IS NULL OR wanting_reply = 0 OR handle IS NOT NULL),
           CHECK (status <> 'unanswered' OR delivered_at IS NOT NULL),
           CHECK ((slot_window IS NULL) = (turn_id IS NULL)),
           CHECK ((closed_at IS NOT NULL) = (status IN ('answered', 'unanswered', 'dropped'))),
           CHECK (status = 'rejected' OR body_chars = length(body)),
           CHECK (tool <> 'send_message' OR length(body) > 0),
           CHECK (notified_at IS NULL OR status IN ('unanswered', 'dropped')),
           CHECK (turn_id IS NULL OR delivery IN ('submit', 'steer')),
           CHECK (status <> 'dropped' OR wanting_reply = 1 OR delivered_at IS NULL),
           CHECK (part = 0 OR (tool = 'reply' AND status <> 'rejected')),
           CHECK (status = 'rejected' OR (reply_to IS NULL) = (hop = 0)),
           CHECK (status = 'rejected' OR (intent = 'new') = (reply_to IS NULL AND scope IS NULL)),
           CHECK (reply_to IS NULL OR scope IS NULL),
           CHECK (scope IS NULL OR tool = 'send_message'),
           CHECK ((scope_turn_id IS NOT NULL) = (scope IS NOT NULL AND status <> 'rejected')),
           CHECK (status = 'rejected' OR intent <> 'continue'),
           CHECK (tool <> 'reply' OR intent = 'reply'),
           CHECK (tool <> 'send_message' OR status = 'rejected' OR intent IN ('new', 'steer')),
           CHECK (tool <> 'app_action' OR status = 'rejected'),
           CHECK (tool <> 'start_session' OR status = 'rejected' OR (intent = 'new' AND wanting_reply = 0 AND to_session_id IS NOT NULL)),
           CHECK (tool IN ('send_message', 'reply') OR intent = 'new'),
           CHECK (intent <> 'steer' OR status = 'rejected' OR delivery IN ('queue', 'steer')),
           CHECK (id GLOB 'm_[a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7][a-z2-7]'),
           CHECK (payload_hash NOT GLOB '*[^0-9a-f]*'),
           CHECK (request_id NOT GLOB '*[^A-Za-z0-9_-]*'),
           CHECK (length(body) = min(body_chars, 8000)),
           CHECK ((gate_result = 'too_large') = (body_chars > 8000))
         );
CREATE UNIQUE INDEX mono_messages_handle
      ON mono_messages (handled_by_session, turn_id, handle) WHERE handle IS NOT NULL;
CREATE INDEX mono_messages_pair
      ON mono_messages (from_mono_id, to_mono_id, to_session_id, created_at)
      WHERE tool = 'send_message' AND status <> 'rejected';
CREATE INDEX mono_messages_session
      ON mono_messages (handled_by_session, status) WHERE handled_by_session IS NOT NULL;
CREATE INDEX mono_messages_from_session
      ON mono_messages (from_session_id, status) WHERE status IN ('open', 'unanswered');
CREATE INDEX mono_messages_waiting
      ON mono_messages (to_mono_id, status) WHERE status IN ('open', 'unanswered');
CREATE INDEX mono_messages_notice
      ON mono_messages (status) WHERE status IN ('unanswered', 'dropped') AND notified_at IS NULL;
CREATE INDEX mono_messages_reply_to
      ON mono_messages (reply_to) WHERE reply_to IS NOT NULL;
CREATE TABLE mono_session_ends (
           session_id TEXT PRIMARY KEY,
           ended_at   INTEGER NOT NULL
         );
INSERT INTO schema_migrations (version, applied_at) VALUES
  (1, 1700000000000), (2, 1700000000000), (3, 1700000000000), (4, 1700000000000),
  (5, 1700000000000), (6, 1700000000000), (7, 1700000000000), (8, 1700000000000),
  (9, 1700000000000), (10, 1700000000000), (11, 1700000000000), (12, 1700000000000),
  (13, 1700000000000), (14, 1700000000000), (15, 1700000000000), (16, 1700000000000),
  (17, 1700000000000), (18, 1700000000000), (19, 1700000000000);
INSERT INTO monos_legacy_import (id, imported_at) VALUES (1, 1700000001000);
INSERT INTO sessions (id, cwd, harness, model, model_settings, runtime_mode, title, blocks_json, created_at, updated_at, has_user_message) VALUES
  ('11111111-1111-4111-8111-111111111111', '@HOME@', 'omp', 'omp:default', '{}', 'auto', 'Alpha', '[]', 1700000001000, 1700000001000, 0),
  ('22222222-2222-4222-8222-222222222222', '@HOME@', 'omp', 'omp:default', '{}', 'auto', 'Beta', '[]', 1700000001000, 1700000001000, 0),
  ('33333333-3333-4333-8333-333333333333', '@A@', 'omp', 'omp:default', '{}', 'auto', 'Before M', '[]', 1700000001000, 1700000001000, 1);
INSERT INTO monos (id, name, role, data_json, sort_order, created_at, updated_at) VALUES
  ('beta', 'Beta', 'mono', json_object('name', 'Beta', 'mascot', 'ghost', 'color', '#22c55e', 'projects', json_array('@B@'), 'sessionId', '22222222-2222-4222-8222-222222222222'), 0, 1700000001000, 1700000001000),
  ('alpha', 'Alpha', 'mono', json_object('name', 'Alpha', 'mascot', 'crab', 'color', '#3b82f6', 'projects', json_array('@A@'), 'sessionId', '11111111-1111-4111-8111-111111111111'), 1, 1700000001000, 1700000001000);
