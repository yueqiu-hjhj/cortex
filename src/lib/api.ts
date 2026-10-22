// Typed Tauri command client. Mirrors src-tauri/src/commands.rs 1:1.
// Every backend command is wrapped here so views never call `invoke` directly.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Source {
  id: string;
  subject_id: string;
  topic_id: string | null;
  name: string;
  kind: string;
  status: string;
  meta: string | null;
  origin: string | null;
  error: string | null;
  content: string | null;      // extracted plaintext (for readable text preview)
  stored_path: string | null;  // stable path to the persisted original/rendered file
  tags: string[];
  created_at: number;
  updated_at: number;
}

export interface Topic {
  id: string;
  subject_id: string;
  name: string;
  glyph: string | null;
  position: number;
  tags: string[];
  sources: Source[];
}

export interface Subject {
  id: string;
  name: string;
  code: string | null;
  glyph: string;
  color: string | null;
  status: string; // ready | review
  streak: number;
  position: number;
  sourceCount: number;
  topics: Topic[];
  created_at: number;
  updated_at: number;
  moodle_course_id?: string | null;
  calendar_aliases?: string | null;
  archived?: boolean;
}

export interface IngestResult {
  source: Source;
  chunk_count: number;
  chars: number;
  warning: string | null;
}

export interface ChunkInfo {
  ord: number;
  text: string;
  dim: number;
  loc: string | null;
}

export interface ChunkHit {
  id: string;
  source_id: string;
  source_name: string;
  text: string;
  loc: string | null;
  score: number;
}

/** One global-search (Ctrl+K) result. For "chunk" hits `id` is the source id. */
export interface SearchHit {
  kind: "chunk" | "source" | "note" | "event" | "material";
  id: string;
  subject_id: string | null;
  title: string;
  snippet: string;
  score: number;
}

export interface IngestProgress {
  source_id: string;
  stage: string; // parsing | chunking | embedding | storing | done | error
  detail: string;
  pct: number;
}

export interface Citation {
  source_id: string;
  chunk_id: string;
  source_name: string;
  loc: string | null;
  /** Character span of the passage in the source's extracted text (0046) —
   *  slice-verifiable: source_text.slice(char_start, char_end) === passage. */
  char_start?: number | null;
  char_end?: number | null;
  snippet: string;
  /** The frozen snapshot (0078): the chunk's full text at answer time.
   *  citesnap re-checks it against the source's current text, so the
   *  evidence can be verified long after re-indexing. Empty on pre-0078
   *  citations (no snapshot was taken). */
  quote?: string;
  score: number;
  cited: boolean;
}
/** Result of re-checking one frozen citation quote against the source's
 *  current text (0078, KnowNote's content==slice invariant). */
export type QuoteCheck =
  | { kind: "exact" }
  | { kind: "moved"; char_start: number; char_end: number }
  | { kind: "drifted" }
  | { kind: "source_gone" };
export interface WebImage {
  img: string;
  thumb: string;
  title: string;
  source: string;
}
export interface ChatAnswer {
  text: string;
  citations: Citation[];
  model: string;
  images?: WebImage[];
  uncited?: boolean;
  /** Retrieval found nothing and web/framework had nothing either — the answer
   *  is general knowledge by construction; every citation token was stripped. */
  abstained?: boolean;
  /** ⟦…⟧ tokens naming sources that were never retrieved — removed from text. */
  fabricated_dropped?: number;
  /** Retrieval accounting persisted with the message (session-level rate). */
  retrieval?: RetrievalMeta;
  /** Terminal generation state (KnowNote borrow, 0077) — why the answer looks
   *  the way it does: truncated answers keep their partial text and invite a
   *  continuation; blocked was stopped by a safety filter; failed means the
   *  model returned nothing usable. */
  answer_state?: AnswerState;
  /** The provider's raw finishReason, verbatim (MAX_TOKENS vs SAFETY …). */
  finish_reason?: string | null;
  /** Set when this answer triggered an auto-rollover (0082): the transcript
   *  had outgrown its budget, was summarized, and continued in a fresh
   *  thread. The UI shows a non-silent notice. */
  rolled_over?: RolloverInfo | null;
}
/** Details of an auto-rollover that happened while answering (0082). */
export interface RolloverInfo {
  /** The archived thread — its history is untouched. */
  old_thread_id: string;
  /** The fresh thread this answer (and its question) live in. */
  new_thread_id: string;
  /** The inherited summary — also persisted as the new thread's first
   *  system message, so it survives reloads. */
  summary: string;
}
/** The five terminal states of one generation. */
export type AnswerState = "completed" | "truncated" | "blocked" | "aborted" | "failed";
export interface RetrievalMeta {
  /** Passages retrieved; 0 = nothing found / not consulted. */
  hits: number;
  /** subject | topic | source | sources | none */
  scope: string;
  abstained: boolean;
}
export interface CsItem { t: string; d: string }
export interface CsSection { id: string; title: string; state: string; items: CsItem[]; image?: string | null }
export interface CheatsheetData {
  subject: string;
  topic: string;
  sources: number;
  /** Sources actually synthesized into this sheet (coverage). */
  sources_used: number;
  model: string;
  sections: CsSection[];
}
export interface CheatsheetVersionMeta {
  id: string;
  created_at: number;
  note: string;
  section_count: number;
}
export interface MaterialRec {
  id: string;
  kind: string;
  title: string;
  topic: string;
  meta: string;
  status: string;
  payload: any;
}

export interface AddSourceInput {
  subject_id: string;
  topic_id?: string | null;
  name?: string | null;
  kind?: string | null;
  text?: string | null;
  path?: string | null;
  url?: string | null;
  tags?: string[];
}

// ---- subjects ----
export const listSubjects = () => invoke<Subject[]>("list_subjects");
export const getSubject = (id: string) => invoke<Subject>("get_subject", { id });
export const createSubject = (name: string, code?: string, glyph?: string, color?: string) =>
  invoke<Subject>("create_subject", { name, code, glyph, color });
export const updateSubject = (
  id: string,
  name: string,
  code?: string,
  glyph?: string,
  color?: string
) => invoke<Subject>("update_subject", { id, name, code, glyph, color });
export const deleteSubject = (id: string) => invoke<string>("delete_subject", { id });
export const archiveSubject = (id: string, archived: boolean) =>
  invoke<void>("archive_subject", { id, archived });
export const listArchivedSubjects = () =>
  invoke<Subject[]>("list_archived_subjects", {});

// ---- topics ----
export const createTopic = (subjectId: string, name: string, glyph?: string, tags?: string[]) =>
  invoke<Subject>("create_topic", { subjectId, name, glyph, tags });
export const updateTopic = (id: string, name: string, subjectId: string, glyph?: string, tags?: string[]) =>
  invoke<Subject>("update_topic", { id, name, subjectId, glyph, tags });
export const reorderSubjects = (ids: string[]) =>
  invoke<Subject[]>("reorder_subjects", { ids });
export const reorderTopics = (subjectId: string, ids: string[]) =>
  invoke<Subject>("reorder_topics", { subjectId, ids });
export const deleteTopic = (id: string, subjectId: string) =>
  invoke<Subject>("delete_topic", { id, subjectId });

// ---- sources ----
export const listSources = (subjectId: string) =>
  invoke<Source[]>("list_sources", { subjectId });
export const getSource = (id: string) => invoke<Source>("get_source", { id });
export const updateSource = (
  id: string,
  name: string,
  topicId?: string | null,
  tags?: string[]
) => invoke<Source>("update_source", { id, name, topicId, tags });
export const deleteSource = (id: string) => invoke<string>("delete_source", { id });
/** Re-run ingestion (re-OCR/re-chunk/re-embed) for an existing source in place. */
export const reingestSource = (id: string) => invoke<IngestResult>("reingest_source", { id });
/** Sources that failed to ingest (error / draft-with-error), across all subjects. */
export const listFailedSources = () => invoke<Source[]>("list_failed_sources");

// ---- live homelab sync (smart row-level merge + binary file sync) ----
export interface SyncStatus {
  enabled: boolean;
  configured: boolean;
  last_at: number; // epoch-ms, 0 = never
  /** Live-sync WebSocket connected — changes propagate within ~a second. */
  live: boolean;
}
export const syncStatus = () => invoke<SyncStatus>("sync_status");
export const syncTest = (url: string, user: string, pass: string) =>
  invoke<boolean>("sync_test", { url, user, pass });
/** Push the local DB (union snapshot) + binary files to the homelab; returns the push timestamp (ms). */
export const syncPush = () => invoke<number>("sync_push");
/** Pull + merge the remote vault into the local DB; returns true if a newer remote was merged. */
export const syncPull = () => invoke<boolean>("sync_pull");

// ---- Moodle integration (experimental) ----
export interface MoodleStatus { configured: boolean; user_id: number; last_sync: number }
export interface MoodleSummary { courses: number; grades: number; deadlines: number; announcements: number }
export interface MoodleCourse { id: string; shortname: string; fullname: string }
export interface MoodleGrade { course_id: string; item_name: string; grade: string; percentage: string; feedback: string }
export interface MoodleDeadline { id: string; course_id: string; name: string; due_at: number; kind: string; status: string; url: string }
export interface MoodleAnnouncement { id: string; course_id: string; subject: string; message: string; posted_at: number; url: string }
export interface MoodleData { courses: MoodleCourse[]; grades: MoodleGrade[]; deadlines: MoodleDeadline[]; announcements: MoodleAnnouncement[] }
/** Connect with username+password (non-SSO). Returns the user's full name. */
export const moodleConnect = (url: string, username: string, password: string) =>
  invoke<string>("moodle_connect", { url, username, password });
/** Connect with a pasted web-services token (SSO sites). Returns the user's full name. */
export const moodleSetToken = (url: string, token: string) =>
  invoke<string>("moodle_set_token", { url, token });
/** Open the Moodle SSO launch flow in a window; token captured via the callback. */
export const moodleLoginSso = (url: string) => invoke<void>("moodle_login_sso", { url });
/** Fired when the SSO login window captures + stores a token. Payload: user's name. */
export const onMoodleSsoDone = (cb: (name: string) => void): Promise<UnlistenFn> =>
  listen<string>("moodle-sso-done", (e) => cb(e.payload));
/** Fired when the SSO login flow fails. Payload: error message. */
export const onMoodleSsoError = (cb: (msg: string) => void): Promise<UnlistenFn> =>
  listen<string>("moodle-sso-error", (e) => cb(e.payload));
export const moodleStatus = () => invoke<MoodleStatus>("moodle_status");
export const moodleDisconnect = () => invoke<void>("moodle_disconnect");
export const moodleSync = () => invoke<MoodleSummary>("moodle_sync");
export const moodleData = () => invoke<MoodleData>("moodle_data");
export const moodleLinkSubject = (subjectId: string, courseId: string | null) =>
  invoke<void>("moodle_link_subject", { subjectId, courseId });
export const moodleAutolink = () => invoke<number>("moodle_autolink");

// ---- OCR (clipboard screenshot → vision model) ----

export interface CardDraft { front: string; back: string }
export const extractFlashcards = (mime: string, base64: string) =>
  invoke<CardDraft[]>("extract_flashcards_from_image", { mime, base64 });
/**
 * Ask the configured text LLM to pull term/definition pairs from a free-form
 * chunk (a snippet pasted in, or a single sources extracted text). The dialog
 * shows the candidates and lets the user cherry-pick which to add.
 */
export const aiSuggestGlossary = (text: string, maxTerms?: number) =>
  invoke<[string, string][]>("ai_suggest_glossary", { text, maxTerms });

/** A glossary candidate generated from a source, with the verbatim sentence that
 *  anchors it (the backend drops any candidate whose quote isn't a real substring
 *  of the source text — same discipline as vocabulary mining). */
export interface GlossaryCandidate {
  term: string;
  definition: string;
  quote: string | null;
}

/** Generate glossary terms straight from a source (no paste). The model must
 *  anchor each term in a sentence copied verbatim from the material. */
export const glossaryGenerateFromSource = (sourceId: string) =>
  invoke<GlossaryCandidate[]>("glossary_generate_from_source", { sourceId });

export const saveFlashcards = (subjectId: string, topicId: string | null, cards: CardDraft[], title: string) =>
  invoke<number>("save_flashcards_to_subject", { subjectId, topicId, cards, title });

/** Transcribe a single image (mime + base64) via the configured vision model.
 *  Returns Markdown text. Used by the Notes "Paste screenshot" flow. */
export const ocrImageBytes = (mime: string, base64: string) =>
  invoke<string>("ocr_image_bytes", { mime, base64 });

// ---- calendar event → subject matching (no AI) ----
export const setSubjectAliases = (subjectId: string, aliases: string) =>
  invoke<number>("set_subject_aliases", { subjectId, aliases });
export const retagCalendarEvents = () => invoke<number>("retag_calendar_events");

// ---- external dependency status ----
export interface DepStatus { name: string; detail: string; present: boolean }
export interface DependencyReport { manager: string; deps: DepStatus[]; install_command: string; note: string }
export const dependencyStatus = () => invoke<DependencyReport>("dependency_status");
export const installDependencies = () => invoke<string>("install_dependencies");
/** "appimage" | "linux-package" | "macos" | "windows" | "unknown" — how Cortex was installed. */
export const installKind = () => invoke<string>("install_kind");
export interface FolderFile { path: string; name: string }
export const listFolderSources = (dir: string) => invoke<FolderFile[]>("list_folder_sources", { dir });

// ---- per-subject module framework ----
export interface FrameworkMeta {
  filename: string;
  chars: number;
  updated_at: number;
  file_path: string | null;
  view_kind: string; // pdf | image | text
}
export const setSubjectFramework = (subjectId: string, path: string) =>
  invoke<FrameworkMeta>("set_subject_framework", { subjectId, path });
export const getSubjectFramework = (subjectId: string) =>
  invoke<FrameworkMeta | null>("get_subject_framework", { subjectId });
export const getSubjectFrameworkText = (subjectId: string) =>
  invoke<string | null>("get_subject_framework_text", { subjectId });
export const clearSubjectFramework = (subjectId: string) =>
  invoke<void>("clear_subject_framework", { subjectId });
export const listChunks = (sourceId: string) =>
  invoke<ChunkInfo[]>("list_chunks", { sourceId });
export const addSource = (input: AddSourceInput) =>
  invoke<IngestResult>("add_source", { input });
/** Copy a just-picked file into app storage and return the stable path. On mobile the
 *  picker's temp path is deleted before the background ingest runs — stage it first. */
export const stageUpload = (path: string) =>
  invoke<string>("stage_upload", { path });

// ---- search / settings / demo ----
export const searchChunks = (query: string, subjectId?: string, k?: number) =>
  invoke<ChunkHit[]>("search_chunks", { query, subjectId, k });
/** Global Ctrl+K search: semantic across all subjects + text over records. */
export const globalSearch = (query: string) =>
  invoke<SearchHit[]>("global_search", { query });
export const getSetting = (key: string) => invoke<string | null>("get_setting", { key });
export const setSetting = (key: string, value: string) =>
  invoke<void>("set_setting", { key, value });

// ---- encrypted secret store (DPAPI on Windows) ----
export const setSecret = (key: string, value: string) =>
  invoke<void>("set_secret", { key, value });
export const getSecret = (key: string) => invoke<string | null>("get_secret", { key });
export const deleteSecret = (key: string) =>
  invoke<void>("delete_secret", { key });
// ---- startup self-check ----
export interface Check {
  name: string;
  ok: boolean;
  detail: string;
  hint?: string | null;
}
export interface SelfCheckReport {
  passed: number;
  failed: number;
  checks: Check[];
  user_version: number;
  known_migrations: number;
}
export const listSecrets = () => invoke<string[]>("list_secrets");

export const startupSelfCheck = () => invoke<SelfCheckReport>("startup_self_check");

export const seedDemo = () => invoke<Subject[]>("seed_demo");
export const envProbe = () => invoke<Record<string, boolean>>("env_probe");

  // ---- local ONNX embeddings (zero-config) ----
  /** Coarse status of the local ONNX model on disk. */
  export type OnnxStatus =
    | { kind: "not_present" }
    | { kind: "downloading"; downloaded: number; total: number }
    | { kind: "ready"; bytes_on_disk: number }
    | { kind: "failed"; reason: string };
  export const onnxStatus = () => invoke<OnnxStatus>("onnx_status");
  /** Kick off a download (idempotent). Returns the final status. */
  export const onnxDownload = () => invoke<OnnxStatus>("onnx_download");
  /** Live download progress for the local ONNX model. Payload: { downloaded, total } bytes. */
  export const onOnnxProgress = (cb: (p: { downloaded: number; total: number }) => void): Promise<UnlistenFn> =>
    listen<{ downloaded: number; total: number }>("onnx-progress", (e) => cb(e.payload));

// ---- AI ----
export const chatAnswer = (
  subjectId: string,
  /** "none" = explicit no-retrieval scope (Gloss 三态): model-only answer. */
  level: "subject" | "topic" | "source" | "none",
  query: string,
  sourceId?: string,
  sourceIds?: string[],
  web?: boolean
) => invoke<ChatAnswer>("chat_answer", { subjectId, level, query, sourceId, sourceIds, web });
export const generateCheatsheet = (subjectId: string, topicId?: string, withImages?: boolean) =>
  invoke<CheatsheetData>("generate_cheatsheet", { subjectId, topicId, withImages });
/** Regenerate every topic's sheet (+ ungrouped "General"), then return the composed whole-subject sheet. */
export const generateSubjectCheatsheet = (subjectId: string, withImages?: boolean) =>
  invoke<CheatsheetData | null>("generate_subject_cheatsheet", { subjectId, withImages });
export const getCheatsheet = (subjectId: string, topicId?: string) =>
  invoke<CheatsheetData | null>("get_cheatsheet", { subjectId, topicId });
/** Whole-subject sheet, composed from the stored per-topic sheets. */
export const getSubjectCheatsheet = (subjectId: string) =>
  invoke<CheatsheetData | null>("get_subject_cheatsheet", { subjectId });
export const updateCheatsheet = (subjectId: string, topicId: string | undefined, sections: CsSection[], snapshot = true) =>
  invoke<void>("update_cheatsheet", { subjectId, topicId, sections, snapshot });
export const listCheatsheetVersions = (subjectId: string, topicId?: string) =>
  invoke<CheatsheetVersionMeta[]>("list_cheatsheet_versions", { subjectId, topicId });
export const getCheatsheetVersion = (versionId: string) =>
  invoke<CsSection[]>("get_cheatsheet_version", { versionId });
/** Restore a stored version as the live sheet (snapshots current first). Returns the restored sheet. */
export const restoreCheatsheetVersion = (versionId: string) =>
  invoke<CheatsheetData>("restore_cheatsheet_version", { versionId });
/** Render a self-contained HTML doc to a PDF at `dest` (headless Chromium). */
export const exportPdf = (html: string, dest: string) =>
  invoke<void>("export_pdf", { html, dest });
/** Copy the whole database to a portable .db file at `dest`. */
export const exportDatabase = (dest: string) =>
  invoke<void>("export_database", { dest });
/** Export a flashcard material to an Anki `.apkg` deck at `dest`; returns card count. */
export const exportAnki = (materialId: string, dest: string) =>
  invoke<number>("export_anki", { materialId, dest });
/** Summary of an Anki `.apkg` import: decks created, cards stored, cards skipped. */
export interface AnkiImportResult {
  deck_count: number;
  card_count: number;
  skipped: number;
}
/** Import an Anki `.apkg` at `path` into a subject as flashcard materials (one
 *  per deck). HTML-stripped, deduped vs existing decks in the subject. */
export const importAnki = (subjectId: string, path: string, topicId?: string) =>
  invoke<AnkiImportResult>("import_anki", { subjectId, topicId, path });
// ---- encrypted homelab backups (age + rclone) ----
export interface BackupStatus {
  age_found: boolean;
  rclone_found: boolean;
  recipient_set: boolean;
  remote_set: boolean;
  last_at: number | null;
  last_dest: string | null;
}
export const backupStatus = () => invoke<BackupStatus>("backup_status");
/** Snapshot → age-encrypt → rclone-upload; returns the remote destination path. */
export const backupNow = () => invoke<string>("backup_now");
/** Reclaim disk space (WAL checkpoint + VACUUM). */
export const optimizeDb = () => invoke<void>("optimize_db", {});

// ---- agent platform (custom agents · skills · tools · runs) ----
export interface Agent {
  id: string; user_id: string; name: string; description: string | null;
  system_prompt: string; skills: string[]; tools: string[];
  model: string | null; temperature: number; max_steps: number;
  enabled: boolean; tool_tier: string; created_at: number; updated_at: number;
}
export interface Skill {
  id: string; user_id: string; name: string; description: string;
  body: string; enabled: boolean; created_at: number; updated_at: number;
}
export interface Tool {
  id: string; user_id: string; name: string; description: string;
  kind: string; schema_json: string; enabled: boolean; built_in: boolean; created_at: number;
}
export interface AgentRun {
  id: string; agent_id: string; user_id: string; status: string;
  input: string; result: string | null; error: string | null;
  steps: number; started_at: number; finished_at: number | null;
  /** Shared-conversation id (0047). "" = legacy ungrouped run. */
  thread_id?: string;
}
export interface AgentMessage {
  id: string; run_id: string; role: string; content: string;
  tool_name: string | null; created_at: number;
  /** Set for thread queries (0047): which agent produced this message. */
  agent_id?: string | null;
  agent_name?: string | null;
}
export const agentsList = () => invoke<Agent[]>("agents_list", {});
export const agentsUpsert = (
  id: string | null, name: string, description: string | null, system_prompt: string,
  skills: string[], tools: string[], model: string | null, temperature: number,
  max_steps: number, enabled: boolean, toolTier?: string
) => invoke<Agent>("agents_upsert", { id, name, description, systemPrompt: system_prompt, skills, tools, model, temperature, maxSteps: max_steps, enabled, toolTier: toolTier ?? null });
export const agentsDelete = (id: string) => invoke<void>("agents_delete", { id });
export const agentsStartRun = (agentId: string, input: string, threadId?: string) =>
  invoke<AgentRun>("agents_start_run", { agentId, input, threadId });
/** Switch agent mid-conversation: same shared thread, context kept (0047). */
export const agentsSwitchAgent = (agentId: string, threadId: string, input: string) =>
  invoke<AgentRun>("agents_switch_agent", { agentId, threadId, input });
/** The shared-conversation transcript (all agents, attributed). */
export const agentsThreadMessages = (threadId: string, limit?: number) =>
  invoke<AgentMessage[]>("agents_thread_messages", { threadId, limit });
/** Execute a run to completion: the tool loop runs on the backend (bounded by
 *  the agent's max_steps) and the finished run comes back. Pass `runId` (from
 *  agentsStartRun) to execute an already-created run so the UI can poll its
 *  transcript while the loop works. Can take a while — show a busy state. */
export const agentsExecuteRun = (agentId: string, input: string, runId?: string) =>
  invoke<AgentRun>("agents_execute_run", { agentId, input, runId: runId ?? null });
export const agentsListRuns = (limit: number) => invoke<AgentRun[]>("agents_list_runs", { limit });
export const agentsListMessages = (runId: string, limit: number) => invoke<AgentMessage[]>("agents_list_messages", { runId, limit });
export const agentsListSkills = () => invoke<Skill[]>("agents_list_skills", {});
export const agentsListTools = () => invoke<Tool[]>("agents_list_tools", {});
export const generateMaterial = (
  subjectId: string,
  kind: "flashcards" | "quiz" | "audio" | "infographic" | "slideshow" | "mindmap" | "matrix" | "guide",
  topicId?: string,
  title?: string,
  customPrompt?: string,
  sourceIds?: string[],
  count?: number
) => invoke<MaterialRec>("generate_material", { subjectId, kind, topicId, title, customPrompt, sourceIds, count });

// ---- 2.7: slides two-round confirm (outline → edit → render → single redo) ----

/** One slide of an editable deck plan: what the slide is + what it must land. */
export interface SlidePlan {
  title: string;
  focus: string;
}
export interface SlidesOutline {
  suggested_title: string;
  slides: SlidePlan[];
}
export interface SlidesProgress {
  idx: number;
  total: number;
  title: string;
}

/** Round 1 — plan the deck (editable outline; nothing saved). */
export const slidesOutline = (
  subjectId: string,
  topicId?: string | null,
  customPrompt?: string,
  sourceIds?: string[],
  count?: number
) =>
  invoke<SlidesOutline>("slides_outline", {
    subjectId,
    topicId: topicId ?? null,
    customPrompt: customPrompt ?? null,
    sourceIds: sourceIds ?? null,
    count: count ?? null,
  });

/** Round 2 — render the confirmed outline, one focused call per slide. */
export const renderSlides = (
  subjectId: string,
  outline: SlidePlan[],
  title?: string,
  topicId?: string | null,
  customPrompt?: string
) =>
  invoke<MaterialRec>("render_slides", {
    subjectId,
    outline,
    title: title ?? null,
    topicId: topicId ?? null,
    customPrompt: customPrompt ?? null,
  });

/** Post-render: redo ONE saved slide (optional steering instruction). */
export const regenerateSlide = (materialId: string, index: number, instruction?: string | null) =>
  invoke<Record<string, unknown>>("regenerate_slide", {
    materialId,
    index,
    instruction: instruction ?? null,
  });

/** Per-slide progress while render_slides is running. */
export const onSlidesProgress = (cb: (p: SlidesProgress) => void): Promise<UnlistenFn> =>
  listen<SlidesProgress>("slides:progress", (e) => cb(e.payload));
/** Synthesize a real audio-overview mp3 from the script segments via cloud TTS.
 *  Returns the file path (serve to <audio> with convertFileSrc). Errors offline /
 *  without an OpenAI key — caller falls back to on-device speech synthesis. */
export const synthesizeOverview = (
  materialId: string,
  segments: { speaker: string; text: string }[],
  force?: boolean
) => invoke<string>("synthesize_overview", { materialId, segments, force });

// ---- Video overview (D1): rendered slides + TTS narration → MP4 ----
export interface VideoSlideInput {
  /** Frontend-canvas-rendered slide, as a data:image/png;base64 URL. */
  png: string;
  title: string;
  bullets: string[];
  notes: string;
}
export interface VideoOverviewResult {
  path: string;
  duration_secs: number;
  slides: number;
  /** True when ffmpeg had to be downloaded this run (first video only). */
  ffmpeg_fetched: boolean;
}
export const synthesizeVideoOverview = (
  materialId: string,
  slides: VideoSlideInput[],
  force?: boolean
) =>
  invoke<VideoOverviewResult>("synthesize_video_overview", {
    materialId,
    slides,
    force,
  });
export const listMaterials = (subjectId: string) =>
  invoke<MaterialRec[]>("list_materials", { subjectId });
export const deleteMaterial = (id: string) => invoke<string>("delete_material", { id });
export const renameMaterial = (id: string, title: string) =>
  invoke<void>("rename_material", { id, title });

// ---- exam mode (timed, locally-graded practice exams) ----
// `questions`/`answers`/`results` are loosely-typed JSON (mirrors the backend's
// serde_json::Value); ExamView narrows them at the point of use.
export interface ExamRec {
  id: string;
  subject_id: string;
  topic_ids: string[];
  title: string;
  duration_min: number;
  questions: any;
  answers: any;
  results: any;
  status: string; // ready | in_progress | graded
  started_ms: number | null;
  score: number | null;
  created_at: number;
  updated_at: number;
}
export interface ExamAnswerInput {
  id: string;
  choice?: number | null;
  text?: string | null;
}
export const generateExam = (
  subjectId: string,
  topicIds: string[] | undefined,
  durationMin: number,
  mcqCount: number,
  writtenCount: number
) =>
  invoke<ExamRec>("generate_exam", { subjectId, topicIds, durationMin, mcqCount, writtenCount });
export const startExam = (id: string) => invoke<ExamRec>("start_exam", { id });
export const submitExam = (id: string, answers: ExamAnswerInput[]) =>
  invoke<any>("submit_exam", { id, answers });
/** Re-grade a finished exam's stored answers (same pipeline/rubric as submit). */
export const remarkExam = (id: string) => invoke<any>("remark_exam", { id });
export const listExams = (subjectId: string) =>
  invoke<ExamRec[]>("list_exams", { subjectId });
export const getExam = (id: string) => invoke<ExamRec>("get_exam", { id });
export const deleteExam = (id: string) => invoke<void>("delete_exam", { id });

// ---- lecture recording (Whisper) ----
export const saveRecording = (
  subjectId: string,
  name: string,
  audio: number[],
  topicId?: string,
  ext?: string
) => invoke<IngestResult>("save_recording", { subjectId, name, audio, topicId, ext });

/** Raw-bytes save: the audio rides the invoke body as-is (no JSON number[]),
 * which is the only sane transport for hour-long recordings (~100MB). Metadata
 * travels in headers (percent-encoded — header values must be ASCII-safe). */
export const saveRecordingRaw = (
  subjectId: string,
  name: string,
  audio: Uint8Array,
  topicId?: string,
  ext?: string,
  diarize?: boolean
) =>
  invoke<IngestResult>("save_recording_raw", audio, {
    headers: {
      "x-subject-id": encodeURIComponent(subjectId),
      "x-name": encodeURIComponent(name),
      ...(topicId ? { "x-topic-id": encodeURIComponent(topicId) } : {}),
      ...(ext ? { "x-ext": encodeURIComponent(ext) } : {}),
      ...(diarize !== undefined ? { "x-diarize": String(diarize) } : {}),
    },
  });

// Near-live transcription: transcribe an audio slice and return its text (or ""
// if no Whisper transcriber is installed). Used by the recorder's live panel.
export const transcribePartial = (audio: number[], ext?: string) =>
  invoke<string>("transcribe_partial", { audio, ext });

// Validate the homelab Whisper setup end to end: server reachable + configured
// model installed (downloading it on the spot if missing). Resolves to a
// human-readable status; rejects with what to fix.
export const checkWhisperModel = () => invoke<string>("check_whisper_model");

// ---- native (iOS) lecture recording ----
// WKWebView's custom-scheme pages are not a secure context, so getUserMedia is
// unavailable on iOS — capture runs natively (AVAudioRecorder) behind these
// commands instead, and keeps recording while the phone is locked.
export const nativeRecStart = () => invoke<void>("native_rec_start");
export const nativeRecPause = () => invoke<void>("native_rec_pause");
export const nativeRecResume = () => invoke<void>("native_rec_resume");
/** Stop and return the recorded file's path + duration (secs). */
export const nativeRecStop = () =>
  invoke<{ path: string; secs: number }>("native_rec_stop");
export const nativeRecCancel = () => invoke<void>("native_rec_cancel");
/** Metering sample: input level 0..1 for the waveform + authoritative elapsed
 * seconds (webview timers freeze while the phone is locked; the recorder's
 * clock doesn't). */
export const nativeRecLevel = () =>
  invoke<{ level: number; secs: number }>("native_rec_level");
/** Delete a stopped-but-unsaved native recording file. */
export const nativeRecDiscardFile = (path: string) =>
  invoke<void>("native_rec_discard", { path });
/** Save a recording that already lives in a backend file (native capture path). */
export const saveRecordingPath = (
  subjectId: string,
  name: string,
  path: string,
  topicId?: string,
  diarize?: boolean
) => invoke<IngestResult>("save_recording_path", { subjectId, name, path, topicId, diarize });

// ---- settings (bulk) ----
export const getAllSettings = () => invoke<Record<string, string>>("get_all_settings");
export const setSettings = (values: Record<string, string>) =>
  invoke<void>("set_settings", { values });

// ---- cron / scheduled automation (3.4) ----
export interface CronJob {
  id: string;
  name: string;
  prompt: string;
  subject_id: string | null;
  kind: string; // "interval" | "daily"
  interval_minutes: number | null;
  daily_time: string | null;
  tz_offset_minutes: number;
  enabled: boolean;
  last_run_at: number | null;
  next_run_at: number;
  action: string; // "prompt" | "sync"
  action_ref: string | null; // connector_id for "sync"
  auto_flashcards: boolean; // sync only: generate + seed due-now SRS cards after pull
}
export interface CronRun {
  id: string;
  job_id: string;
  started_at: number;
  finished_at: number | null;
  status: string; // running | ok | error
  source_id: string | null;
  output: string | null;
  error: string | null;
}
export interface CronJobIn {
  name: string;
  prompt: string;
  subject_id: string | null;
  kind: string;
  interval_minutes: number | null;
  daily_time: string | null;
  tz_offset_minutes?: number;
  action?: string; // "prompt" | "sync"
  action_ref?: string | null; // connector_id for "sync"
  auto_flashcards?: boolean; // sync only
}
export const cronList = () => invoke<CronJob[]>("cron_list");
export const cronAdd = (job: CronJobIn) => invoke<CronJob>("cron_add", { job });
export const cronUpdate = (id: string, job: CronJobIn) =>
  invoke<CronJob>("cron_update", { id, job });
export const cronSetEnabled = (id: string, enabled: boolean) =>
  invoke<CronJob>("cron_set_enabled", { id, enabled });
export const cronDelete = (id: string) => invoke<void>("cron_delete", { id });
export const cronRuns = (jobId: string) => invoke<CronRun[]>("cron_runs", { jobId });
export const cronRunNow = (id: string) => invoke<CronRun>("cron_run_now", { id });
export interface CronRecentRun {
  job_id: string;
  job_name: string;
  action: string; // "prompt" | "sync"
  status: string; // "ok" | "error"
  finished_at: number | null;
  output: string | null;
}
export const cronRecentRuns = (limit = 3) =>
  invoke<CronRecentRun[]>("cron_recent_runs", { limit });

// ---- MCP client (4.4) ----
export interface McpSyncReport {
  server: string;
  ok: boolean;
  tools: number;
  error: string | null;
}
export const mcpSyncTools = () => invoke<McpSyncReport[]>("mcp_sync_tools");

// ---- chat history (persisted per subject) ----
export interface ChatMsg {
  role: string;
  text: string;
  citations?: Citation[];
  created_at: number;
  /** NULL for user/system rows and pre-0044 messages. */
  retrieval?: RetrievalMeta | null;
  /** Fabriced citations citecheck stripped from this answer (0069). */
  fabricated_dropped?: number | null;
  /** Terminal generation state (0077). NULL for user/system rows and
   *  pre-0077 messages — those predate the state machine, so no badge. */
  answer_state?: AnswerState | null;
}
export const listChatMessages = (subjectId: string) =>
  invoke<ChatMsg[]>("list_chat_messages", { subjectId });
export const addChatMessage = (
  subjectId: string,
  role: string,
  text: string,
  citations?: Citation[],
  retrieval?: RetrievalMeta,
  /** How many fabricated citations citecheck stripped from this answer (0069). */
  fabricatedDropped?: number,
  /** Terminal generation state (0077) — set by the caller because only it
   *  knows about user aborts; the backend classifies provider stops. */
  answerState?: AnswerState
) =>
  invoke<void>("add_chat_message", {
    subjectId,
    role,
    text,
    citations,
    retrieval,
    fabricatedDropped: fabricatedDropped ?? null,
    answerState: answerState ?? null,
  });
export const clearChat = (subjectId: string) => invoke<void>("clear_chat", { subjectId });
/** Re-check one answer's frozen citation quotes against current source text
 *  (0078). One status per citation, in citation order; null entries are
 *  pre-0078 citations with no snapshot. Empty when no such message. */
export const verifyCitationSnapshots = (subjectId: string, createdAt: number) =>
  invoke<(QuoteCheck | null)[]>("verify_citation_snapshots", { subjectId, createdAt });

// ---- reverse provenance (KnowNote borrow) ----
/** One answer that cited a passage, with the question that provoked it. */
export interface AnswerRef {
  question: string;
  answer: string;
  created_at: number;
  thread_id: string;
}
/** One passage of a source that at least one answer cited inline. */
export interface PassageBackref {
  chunk_id: string;
  ord: number;
  loc: string | null;
  excerpt: string;
  cites: AnswerRef[];
}
/** One asked question with its paired answer's provenance. */
export interface QuestionEntry {
  question: string;
  created_at: number;
  thread_id: string;
  /** Terminal generation state of the paired answer (0077), if any. */
  answer_state?: AnswerState | null;
  /** Retrieval hits of the paired answer. */
  hits?: number | null;
  /** Fabricated citations citecheck stripped from the paired answer. */
  fabricated_dropped?: number | null;
  /** How many citations the answer's evidence panel showed. */
  citation_count: number;
}
export const sourceBackrefs = (subjectId: string, sourceId: string) =>
  invoke<PassageBackref[]>("source_backrefs", { subjectId, sourceId });
export const questionIndex = (subjectId: string, limit?: number) =>
  invoke<QuestionEntry[]>("question_index", { subjectId, limit });

// chat sessions (history)
export interface ThreadInfo { id: string; title: string; updated_at: number; count: number }
export const newChat = (subjectId: string) => invoke<void>("new_chat", { subjectId });
export const listChatThreads = (subjectId: string) =>
  invoke<ThreadInfo[]>("list_chat_threads", { subjectId });
export const openChatThread = (subjectId: string, threadId: string) =>
  invoke<void>("open_chat_thread", { subjectId, threadId });

// ---- web search (SearXNG on the user's homelab) ----
export interface WebResult {
  title: string;
  url: string;
  host: string;
  snippet: string;
  engine: string;
  img_src?: string | null;
  thumbnail?: string | null;
}
export const webSearch = (query: string, categories?: string) =>
  invoke<WebResult[]>("web_search", { query, categories });

// ---- literature search (multi-source + RRF + rule rerank) ----
export interface LiteratureHit {
  title: string;
  authors: string[];
  year?: number | null;
  venue?: string | null;
  doi?: string | null;
  url?: string | null;
  abstract_text?: string | null;
  citation_count?: number | null;
  source: string;
  sources: string[];
  rrf_score: number;
  final_score: number;
  reasons: string[];
}

export interface LiteratureResult {
  query: string;
  rewritten: string[];
  hits: LiteratureHit[];
  sources_attempted: string[];
  sources_succeeded: string[];
  took_ms: number;
}

/**
 * Literature: selection LLM + repair + save-to-source helpers.
 */
/** Run a focused LLM call for selection-based reading assistance. */
export const literatureExplain = (text: string, mode: string) =>
  invoke<string>("literature_explain", { text, mode });

/** Repair PDF/OCR paper text: merge hyphenated line breaks, fold soft wraps, collapse blanks. */
export const literatureRepair = (text: string) =>
  invoke<string>("literature_repair", { text });

/** 2.6 公式修复 — make extracted math KaTeX-renderable (\\[..\\] → $$..$$,
 *  wrap equation-like bare lines into display blocks). Pure heuristic. */
export const literatureFixMath = (text: string) =>
  invoke<string>("literature_fix_math", { text });

/** 2.6 图表自动解释 — explain the figure(s) on one PDF page (vision model),
 *  guided by the caption text the user selected. `path` is the source's
 *  stored_path; `page` is 1-based. */
export const figureExplain = (path: string, page: number, caption?: string) =>
  invoke<string>("figure_explain", { path, page, caption: caption ?? null });

/** Persist a paper as a text-kind source under a subject. Returns the new source id. */
export const literatureSaveSource = (
  subjectId: string,
  title: string,
  text: string,
  url?: string | null,
  topicId?: string | null,
) =>
  invoke<string>("literature_save_source", {
    subjectId,
    title,
    text,
    url: url ?? null,
    topicId: topicId ?? null,
  });

// ---- vocabulary workbench (per-subject corpus) ----
export interface VocabTerm {
  id: string;
  subject_id: string;
  term: string;
  normalized: string;
  definition: string | null;
  examples: string | null;
  freq: number;
  mastery: number;
  /** new | learning | known */
  status: string;
  created_at: number;
  updated_at: number;
}

export const vocabList = (subjectId: string) =>
  invoke<VocabTerm[]>("vocab_list", { subjectId });

export const vocabAdd = (subjectId: string, term: string) =>
  invoke<string>("vocab_add", { subjectId, term });

export const vocabUpdate = (
  id: string,
  definition: string | null,
  examples: string | null,
  status: string | null,
) =>
  invoke<void>("vocab_update", { id, definition, examples, status });

export const vocabDelete = (id: string) =>
  invoke<string>("vocab_delete", { id });

/** Scan the subject corpus, extract the top-N most frequent English tokens, and upsert. */
export const vocabExtract = (subjectId: string, limit?: number) =>
  invoke<number>("vocab_extract", { subjectId, limit: limit ?? null });

// ---- corpus mining — the model picks, the sentence is checked verbatim -----

export interface MineCandidate {
  word: string;
  normalized: string;
  definition: string;
  /** A verbatim sentence from the material (backend-checked substring). */
  sentence: string;
}

/** Ask the model which words in this text are worth learning. */
export const vocabMine = (text: string) =>
  invoke<{ candidates: MineCandidate[]; dropped: number }>("vocab_mine", { text });

/** Save picked words as terms + flashcards seeded into the review queue. */
export const vocabSaveMined = (subjectId: string, candidates: MineCandidate[]) =>
  invoke<{ saved: number; seeded: number }>("vocab_save_mined", {
    subjectId,
    candidates,
  });

export type LiteratureSort = "relevance" | "newest" | "cited";

export const literatureSearch = (
  query: string,
  subjectId?: string | null,
  limit?: number,
  sort?: LiteratureSort,
) =>
  invoke<LiteratureResult>("literature_search", {
    query,
    subjectId: subjectId ?? null,
    limit: limit ?? null,
    sort: sort ?? "relevance",
  });

/** AI deep search: LLM expands the question into multi-angle queries, runs
 *  them across every connector, then reranks the merged hits with reasons. */
export interface AiSearchStep {
  query: string;
  hits: number;
}
export interface AiSearchResult {
  hits: LiteratureHit[];
  queries: AiSearchStep[];
  took_ms: number;
}
export const literatureAiSearch = (
  question: string,
  subjectId?: string | null,
  limit?: number,
) =>
  invoke<AiSearchResult>("literature_ai_search", {
    question,
    subjectId: subjectId ?? null,
    limit: limit ?? null,
  });

// ---- Deep Research: topic → discover → screen → import → overview note ----
export interface DeepResearchPick {
  paper_id: string;
  title: string;
  url: string | null;
  doi: string | null;
  year: number | null;
  venue: string | null;
  /** Discovery sources that surfaced it (arxiv / crossref / …). */
  found_by: string;
  /** The screener's one-line reason (or the honest fallback note). */
  why: string;
  /** false = the DOI was already in the library. */
  imported: boolean;
}
export interface DeepResearchResult {
  topic: string;
  rewritten: string[];
  candidates: number;
  sources_attempted: string[];
  sources_succeeded: string[];
  picks: DeepResearchPick[];
  overview: string;
  note_id: string;
  took_ms: number;
  warnings: string[];
}
export const deepResearch = (subjectId: string, topic: string, maxSources?: number) =>
  invoke<DeepResearchResult>("deep_research", {
    subjectId,
    topic,
    maxSources: maxSources ?? null,
  });

// ---- D4: plan-block time-blocking write-back to Google Calendar ----
export interface PlanSyncResult {
  /** New Google events created for planned blocks. */
  pushed: number;
  /** Existing events PATCHed because the block changed locally. */
  updated: number;
  /** Events deleted because the block became done/skipped/moved. */
  removed: number;
  /** Blocks whose times moved IN Google — pulled back into the plan. */
  pulled: number;
  /** Google events whose block vanished locally — deleted. */
  cleaned: number;
  notes: string[];
}
export const planCalendarSync = (tzOffsetMinutes: number, days?: number) =>
  invoke<PlanSyncResult>("plan_calendar_sync", {
    tzOffsetMinutes,
    days: days ?? null,
  });

// ---- Zotero import: read the LOCAL Zotero library into the papers table ----
export interface ZoteroImportResult {
  db_path: string;
  found: number;
  imported: number;
  skipped: number;
  warnings: string[];
  titles_sample: string[];
}
export const zoteroImport = (subjectId?: string | null, dbPath?: string | null) =>
  invoke<ZoteroImportResult>("zotero_import", {
    subjectId: subjectId ?? null,
    dbPath: dbPath ?? null,
  });

// ---- Zotero incremental sync (2.2): refresh linked papers, link by DOI, unlink trash ----
export interface ZoteroSyncResult {
  db_path: string;
  changed: number;
  updated: number;
  added: number;
  unlinked: number;
  warnings: string[];
  titles_sample: string[];
  last_sync_ms: number;
}
export interface ZoteroSyncState {
  last_sync_ms: number | null;
}
export const zoteroSync = (subjectId?: string | null, dbPath?: string | null) =>
  invoke<ZoteroSyncResult>("zotero_sync", {
    subjectId: subjectId ?? null,
    dbPath: dbPath ?? null,
  });
export const zoteroSyncState = () =>
  invoke<ZoteroSyncState>("zotero_sync_state");

// ---- Experiments (科研一体化 gap: 复现实验, lightweight cards) ----
export type ExperimentStatus = "planned" | "running" | "done" | "failed";
export interface Experiment {
  id: string;
  paper_id: string | null;
  subject_id: string | null;
  title: string;
  hypothesis: string | null;
  code: string | null;
  environment: string | null;
  result: string | null;
  status: ExperimentStatus;
  created_at: number;
  updated_at: number;
}
export const experimentList = (paperId?: string | null, subjectId?: string | null) =>
  invoke<Experiment[]>("experiment_list", {
    paperId: paperId ?? null,
    subjectId: subjectId ?? null,
  });
export const experimentCreate = (
  paperId: string | null,
  subjectId: string | null,
  title: string,
  hypothesis?: string | null,
  environment?: string | null,
) =>
  invoke<Experiment>("experiment_create", {
    paperId, subjectId, title, hypothesis: hypothesis ?? null, environment: environment ?? null,
  });
export const experimentUpdate = (
  id: string,
  patch: Partial<Pick<Experiment, "title" | "hypothesis" | "code" | "environment" | "result" | "status">>,
) => invoke<Experiment>("experiment_update", { id, patch });
export const experimentDelete = (id: string) => invoke<void>("experiment_delete", { id });

// ---- ICS export (RFC 5545) — local calendar interop without the cloud ----
export const exportCalendarIcs = (
  dest: string,
  subjectId?: string | null,
  fromMs?: number | null,
  toMs?: number | null,
) =>
  invoke<number>("export_calendar_ics", {
    dest,
    subjectId: subjectId ?? null,
    fromMs: fromMs ?? null,
    toMs: toMs ?? null,
  });

// ---- portable archive `.cortex-pkg.tar.gz` (P0 #2) ----
export interface PkgCounts {
  subjects: number;
  sources: number;
  chunks: number;
  notes: number;
  papers: number;
}
export interface PkgExportResult {
  out: string;
  files: number;
  bytes: number;
  orphans: string[];
  sha256: string;
  counts: PkgCounts;
}
export interface PkgVerifyResult {
  ok: boolean;
  files: number;
  problems: string[];
  counts: PkgCounts | null;
  created_at: number | null;
  app_version: string | null;
}
export interface PkgImportResult {
  files: number;
  counts: PkgCounts;
  repointed: number;
}
export const pkgExport = (dest: string, forceOrphans?: boolean) =>
  invoke<PkgExportResult>("pkg_export", { dest, forceOrphans: forceOrphans ?? null });
export const pkgVerify = (path: string) =>
  invoke<PkgVerifyResult>("pkg_verify", { path });
export const pkgImport = (path: string, targetDir: string, force?: boolean) =>
  invoke<PkgImportResult>("pkg_import", { path, targetDir, force: force ?? null });

/** Export the knowledge framework as a real .xmind (2020+ format). */
export const exportKnowledgeXmind = (subjectId: string, dest: string) =>
  invoke<number>("export_knowledge_xmind", { subjectId, dest });

/** Backfill char offsets for pre-0046 chunks (idempotent; returns count). */
export const backfillChunkOffsets = () => invoke<number>("backfill_chunk_offsets");

// ---- 3.4: cross-book skill cards (0051) ----
export interface SkillCandidate { concept: string; title: string; books: [string, string][] }
export interface SkillCard {
  id: string; concept: string; title: string; summary: string; when_to_use: string;
  steps: string[]; appearances: [string, string][]; model: string; created_at: number;
}
export const skillCardCandidates = (limit?: number) =>
  invoke<SkillCandidate[]>("skill_card_candidates", { limit: limit ?? null });
export const skillCardsList = () => invoke<SkillCard[]>("skill_cards_list");
export const skillCardDelete = (id: string) => invoke<void>("skill_card_delete", { id });
export const skillCardsPrecipitate = (limit?: number) =>
  invoke<[number, number]>("skill_cards_precipitate", { limit: limit ?? null });

// ---- 3.3: cross-paper tension analysis (0050) ----
export interface TensionRun {
  id: string; title: string; status: "running" | "done" | "error";
  error: string | null; item_ids: string[]; created_at: number; updated_at: number;
}
export interface Tension {
  id: string; run_id: string; topic: string;
  paper_a_id: string; paper_b_id: string; claim_a: string; claim_b: string;
  conflict: string; severity: string; quote_a: string; quote_b: string; created_at: number;
}
export interface TensionResult {
  run: TensionRun;
  papers: [string, string][];
  tensions: Tension[];
}
export const tensionRunCreate = (itemIds: string[], title?: string) =>
  invoke<string>("tension_run_create", { itemIds, title });
export const tensionRunList = () => invoke<TensionRun[]>("tension_run_list");
export const tensionRunResult = (runId: string) =>
  invoke<TensionResult>("tension_run_result", { runId });
export const tensionRunDelete = (runId: string) =>
  invoke<void>("tension_run_delete", { runId });
export const tensionRunExecute = (runId: string) =>
  invoke<void>("tension_run_execute", { runId });
export const tensionExportMd = (runId: string) =>
  invoke<string>("tension_export_md", { runId });
export const tensionMdFile = (runId: string, dest: string) =>
  invoke<void>("tension_md_file", { runId, dest });

// ---- red-team review — read your paper the way a hostile reviewer will ---

export interface RedteamRun {
  id: string;
  title: string;
  status: string;
  error: string | null;
  paper_id: string;
  paper_title: string;
  venue: string;
  soundness: number | null;
  novelty: number | null;
  rigor: number | null;
  /** accept | minor | major | reject */
  verdict: string | null;
  summary: string | null;
  created_at: number;
  updated_at: number;
}
export interface RedteamFinding {
  id: string;
  /** fatal | weakness | rescue | rebuttal */
  kind: string;
  title: string;
  detail: string;
  quote: string | null;
  created_at: number;
}
export interface RedteamResult {
  run: RedteamRun;
  findings: RedteamFinding[];
}
export const redteamRunCreate = (paperId: string, venue?: string) =>
  invoke<string>("redteam_run_create", { paperId, venue });
export const redteamRunList = () => invoke<RedteamRun[]>("redteam_run_list");
export const redteamRunResult = (runId: string) =>
  invoke<RedteamResult>("redteam_run_result", { runId });
export const redteamRunDelete = (runId: string) =>
  invoke<void>("redteam_run_delete", { runId });
export const redteamRunExecute = (runId: string) =>
  invoke<void>("redteam_run_execute", { runId });
export const redteamExportMd = (runId: string) =>
  invoke<string>("redteam_export_md", { runId });
export const redteamMdFile = (runId: string, dest: string) =>
  invoke<void>("redteam_md_file", { runId, dest });

// ---- token accounting (0049, estimates) ----
export interface TokenDayBucket { day: string; prompt: number; completion: number; calls: number }
export interface TokenBucket { kind: string; total: number; calls: number }
export interface TokenSummary {
  days: number; total_prompt: number; total_completion: number; calls: number;
  by_day: TokenDayBucket[]; by_kind: TokenBucket[]; by_model: TokenBucket[];
}
export const tokenUsageSummary = (days?: number) =>
  invoke<TokenSummary>("token_usage_summary", { days: days ?? null });

// ---- recovery directories (.deleted-<id>) ----
export interface DeletedSubject {
  dir_name: string;
  path: string;
  subject_name: string;
  deleted_at: number;
  sources: number;
  notes: number;
  bytes: number;
}
export const recoveryList = () =>
  invoke<DeletedSubject[]>("recovery_list");
export const recoveryRestore = (dirName: string) =>
  invoke<[string, number, number]>("recovery_restore", { dirName });
export const recoveryPurge = (dirName: string) =>
  invoke<void>("recovery_purge", { dirName });

// Current Omarchy theme name (null if Omarchy isn't installed). Powers the
// "Follow Omarchy theme" toggle in Appearance.
export const omarchyTheme = () => invoke<string | null>("omarchy_theme");

// ---- long-term memory (manual; injected into AI prompts) ----
export interface Memory {
  layer: "declarative" | "inferred";
  confidence: number;
  confirmed: boolean;
  id: string;
  content: string;
  source: string | null;
  created_at: number;
  updated_at: number;
}
export const listMemory = () => invoke<Memory[]>("list_memory");
export const addMemory = (content: string, layer = "declarative", confidence = 1, source: string | null = null) => invoke<Memory>("add_memory", { content, layer, confidence, source });
export const confirmMemory = (id: string) => invoke<void>("confirm_memory", { id });
export const deleteMemory = (id: string) => invoke<void>("delete_memory", { id });

// ---- custom music stations + YouTube-audio streaming (mpv sidecar) ----
export interface CustomStation {
  id: string;
  name: string;
  url: string;
  kind: string; // "youtube" | "live"
  position: number;
  created_at: number;
}
export interface MediaTools {
  mpv: boolean;
  ffmpeg: boolean;
  ytdlp: boolean;
  ytdlp_path: string;
}
export const listCustomStations = () => invoke<CustomStation[]>("list_custom_stations");
export const addCustomStation = (name: string, url: string, kind = "youtube") =>
  invoke<CustomStation>("add_custom_station", { name, url, kind });
export const deleteCustomStation = (id: string) =>
  invoke<void>("delete_custom_station", { id });
export const reorderCustomStations = (ids: string[]) =>
  invoke<void>("reorder_custom_stations", { ids });
export const mediaToolsStatus = () => invoke<MediaTools>("media_tools_status");
export const youtubePlay = (url: string, volume: number) =>
  invoke<void>("youtube_play", { url, volume });
export const youtubePause = () => invoke<void>("youtube_pause");
export const youtubeResume = () => invoke<void>("youtube_resume");
export const youtubeStop = () => invoke<void>("youtube_stop");
export const youtubeSetVolume = (volume: number) =>
  invoke<void>("youtube_set_volume", { volume });
// Fire-and-forget: pre-resolve station URLs in the background so their first
// play is near-instant (the mpv sidecar caches the resolved direct stream).
export const youtubePrewarm = (urls: string[]) =>
  invoke<void>("youtube_prewarm", { urls });

// ---- data & privacy / homelab utilities ----
export interface DbStats {
  db_bytes: number;
  subjects: number;
  sources: number;
  chunks: number;
}
export const dbStats = () => invoke<DbStats>("db_stats");
export const deleteAllData = () => invoke<void>("delete_all_data");
export const pingUrl = (url: string) => invoke<boolean>("ping_url", { url });

/** Models actually installed on the configured Ollama server (local or homelab). Empty when unreachable / none pulled. */
export const ollamaModels = () => invoke<string[]>("ollama_models");
/** Lightweight authenticated probe of a provider's stored key/url. provider: gemini|openrouter|openai|claude|custom|ollama|<registry-id>. */
export interface VerifyResult { ok: boolean; detail: string }
export const verifyProvider = (provider: string) =>
  invoke<VerifyResult>("verify_provider", { provider });

/** The OpenAI-compatible provider registry (DeepSeek, Qwen, GLM, Kimi, …). The
 *  Settings UI renders model dropdowns and key rows straight from this table —
 *  adding a provider in Rust is the whole change, no frontend edit needed. */
export interface ProviderInfo {
  id: string;
  label: string;
  key_note: string;
  embed_model: string | null;
  chat_models: [string, string][];
}
export const listProviders = () => invoke<ProviderInfo[]>("list_providers");

/** plugins/*.json manifests — the contributor entry point. Providers merge into
 *  the model dropdowns, material_types into the generator presets, leader_keys
 *  into the Space menu. A manifest can contribute data, never code. */
export interface PluginProvider {
  id: string;
  label: string;
  base_url: string;
  key_note: string;
  embed_model: string | null;
  embed_dim: number | null;
  chat_models: [string, string][];
}
export interface PluginMaterialType {
  id: string;
  label: string;
  base_kind: string;
  prompt: string;
}
export interface PluginLeaderKey {
  key: string;
  label: string;
  detail: string;
  view: string;
}
export interface Plugin {
  name: string;
  file: string;
  providers: PluginProvider[];
  material_types: PluginMaterialType[];
  leader_keys: PluginLeaderKey[];
  connectors: ConnectorInfo[];
}
export interface PluginList { plugins: Plugin[]; errors: string[] }
export const pluginsList = () => invoke<PluginList>("plugins_list");

// ---- Google Calendar (OAuth + sync) ----
export interface GoogleStatus {
  connected: boolean;
  email: string | null;
  configured: boolean;
}
export interface SyncResult {
  pulled: number;
  pushed: number;
}
export const googleStatus = () => invoke<GoogleStatus>("google_status");
export const googleConnect = () => invoke<GoogleStatus>("google_connect");
export const googleDisconnect = () => invoke<GoogleStatus>("google_disconnect");
export const googleSync = () => invoke<SyncResult>("google_sync");
export interface GoogleCalendar { id: string; summary: string; primary: boolean; selected: boolean; color: string }
export const googleListCalendars = () => invoke<GoogleCalendar[]>("google_list_calendars");

// ---- in-app reader browsing (inside Web search) ----
export interface PageLink { href: string; text: string }
export interface FetchedPage {
  url: string;
  final_url: string;
  title: string;
  text: string;
  links: PageLink[];
}
export const fetchPage = (url: string) => invoke<FetchedPage>("fetch_page", { url });

// ---- source re-filing ----
export const moveSource = (id: string, subjectId: string, topicId?: string | null) =>
  invoke<Source>("move_source", { id, subjectId, topicId });

// ---- notes ----
export interface Note {
  id: string;
  subject_id: string | null;
  topic_id: string | null;
  title: string;
  body: string;
  source_id: string | null;
  created_at: number;
  updated_at: number;
  /** Markdown file backing this note, relative to the vault root. */
  vault_path: string | null;
}
export const createNote = (
  title: string,
  body: string,
  subjectId?: string | null,
  topicId?: string | null
) => invoke<Note>("create_note", { subjectId, topicId, title, body });
export const listNotes = (subjectId?: string | null) =>
  invoke<Note[]>("list_notes", { subjectId });
export const getNote = (id: string) => invoke<Note>("get_note", { id });
export const updateNote = (id: string, title: string, body: string) =>
  invoke<Note>("update_note", { id, title, body });

// ---- wiki-style [[double links]] between notes (0045) ----
export interface Backlink {
  note_id: string;
  title: string;
  alias: string | null;
}
export const noteBacklinks = (title: string) =>
  invoke<Backlink[]>("note_backlinks", { title });
export const noteResolveLink = (target: string) =>
  invoke<[string, string] | null>("note_resolve_link", { target });
export const deleteNote = (id: string) => invoke<void>("delete_note", { id });
export const noteToSource = (id: string) =>
  invoke<IngestResult>("note_to_source", { id });

// ---- vault layer: notes as Markdown files on disk ----
export interface VaultStatus {
  root: string;
  files: number;
  notes: number;
  on_disk: number;
  custom: boolean;
}
export const vaultStatus = () => invoke<VaultStatus>("vault_status");
/** Point the vault at a directory (e.g. an existing Obsidian vault); "" resets. */
export const vaultSetPath = (path: string) =>
  invoke<VaultStatus>("vault_set_path", { path });
/** Rescan the vault now — picks up files edited outside the app. */
export const vaultReindex = () => invoke<VaultStatus>("vault_reindex");

// ---- agent write-back: drafts waiting for review (L2) ----
// An MCP client (Codex / Claude Code) can only *propose* a note. It lands here
// with a line diff; the note and its .md file are written when you accept.
export type ProposalKind = "note_new" | "note_append" | "note_replace";
export interface ProposalDiffLine {
  kind: "same" | "add" | "del" | "gap";
  text: string;
  old_no: number | null;
  new_no: number | null;
}
export interface ProposalPreview {
  lines: ProposalDiffLine[];
  added: number;
  removed: number;
  truncated: boolean;
}
export interface Proposal {
  id: string;
  kind: ProposalKind;
  status: "pending" | "accepted" | "rejected" | "withdrawn";
  note_id: string | null;
  title: string;
  subject_id: string | null;
  rationale: string;
  author: string;
  created_at: number;
  resolved_at: number | null;
  result_note_id: string | null;
  preview: ProposalPreview;
}
export interface AppliedProposal {
  note_id: string;
  title: string;
  vault_path: string | null;
  vault_error: string | null;
}
/** Pending proposals by default; pass "all" for the history. */
export const listProposals = (status?: string | null) =>
  invoke<Proposal[]>("list_proposals", { status: status ?? null });
/** Apply a draft. `force` overwrites a note that changed since the draft. */
export const acceptProposal = (id: string, force = false) =>
  invoke<AppliedProposal>("accept_proposal", { id, force });
export const rejectProposal = (id: string) => invoke<void>("reject_proposal", { id });
/** Cheap count for the Today flow bar (no diffs). */
export const proposalPendingCount = () => invoke<number>("pending_proposals");

// ---- clips: select a passage anywhere → file it under a note (L3) ---------
// Distinct from `excerpts` (paper evidence, always tied to a paper/source and
// citable) and from the browser clip server (whole-page URL capture). A clip is
// one selected passage with no owner until you give it one.
export type ClipKind = "chat" | "agent" | "note" | "paper" | "source" | "web" | "manual";

export interface ClipRequest {
  quote: string;
  sourceKind: ClipKind;
  /** Set when the passage is already a clip (filing from the inbox) — the
   *  picker then files that row instead of capturing it a second time. */
  clipId?: string | null;
  sourceId?: string | null;
  /** What the reader should see: "AI 回答 · deepseek", "《TCP 详解》". */
  sourceLabel?: string | null;
  page?: number | null;
}

export interface Clip {
  id: string;
  quote: string;
  source_kind: string;
  source_id: string | null;
  source_label: string | null;
  page: number | null;
  /** null = still in the inbox (not yet filed under a note). */
  note_id: string | null;
  /** The exact Markdown written into the note — used by undo. */
  block: string | null;
  created_at: number;
  note_title: string | null;
  /** How many flashcards this passage already produced (0 = not carded yet). */
  card_count: number;
}

export interface ClipDiscardOutcome {
  /** True when the passage was also taken back out of the note. */
  reverted: boolean;
}

export const clipAdd = (req: ClipRequest) =>
  invoke<Clip>("clip_add", {
    quote: req.quote,
    sourceKind: req.sourceKind,
    sourceId: req.sourceId ?? null,
    sourceLabel: req.sourceLabel ?? null,
    page: req.page ?? null,
  });
export const clipsList = (pendingOnly = true) =>
  invoke<Clip[]>("clips_list", { pendingOnly });
/** File under an existing note — lands at the end of its `## 摘录` section. */
export const clipToNote = (clipId: string, noteId: string) =>
  invoke<Clip>("clip_to_note", { clipId, noteId });
export const clipToNewNote = (clipId: string, title: string, subjectId?: string | null) =>
  invoke<Clip>("clip_to_new_note", { clipId, title, subjectId: subjectId ?? null });
/** Forget it. When the note still holds the block verbatim, it is removed too. */
export const clipDiscard = (clipId: string) =>
  invoke<ClipDiscardOutcome>("clip_discard", { clipId });

/** A passage → self-test questions. Drafts only; nothing is scheduled until
 *  `clipSaveCards`, so a bad guess costs one call instead of review clutter. */
export interface ClipCardDraft {
  front: string;
  back: string;
}
export interface ClipCardsSaved {
  material_id: string;
  subject_id: string;
  cards: number;
  /** Cards actually added to the queue — less than `cards` when that front was
   *  already scheduled (the scheduler keeps your progress). */
  seeded: number;
}
export const clipDraftCards = (clipId: string) =>
  invoke<ClipCardDraft[]>("clip_draft_cards", { clipId });
export const clipSaveCards = (
  clipId: string,
  cards: ClipCardDraft[],
  subjectId?: string | null,
  topicId?: string | null,
) =>
  invoke<ClipCardsSaved>("clip_save_cards", {
    clipId,
    cards,
    subjectId: subjectId ?? null,
    topicId: topicId ?? null,
  });

// ---- calendar (events + tasks) ----
export interface CalEvent {
  id: string;
  subject_id: string | null;
  title: string;
  description: string | null;
  location: string | null;
  color: string | null;
  start_ms: number;
  end_ms: number | null;
  all_day: boolean;
  kind: string; // event | task | exam | assignment | project
  done: boolean;
  reminder_ms: number | null;
  notified: boolean;
  google_id: string | null;
  tags: string[];
  checklist: string[]; // ticked topic ids (deadline study checklist)
  priority: string | null; // assignment priority: low | med | high (null = normal)
  topic_ids: string[]; // covered topic ids (assignments)
  status: string; // kanban column: todo | doing | done
  created_at: number;
  updated_at: number;
}
export const createEvent = (e: {
  title: string;
  startMs: number;
  subjectId?: string | null;
  description?: string | null;
  location?: string | null;
  color?: string | null;
  endMs?: number | null;
  allDay?: boolean;
  kind?: string;
  reminderMs?: number | null;
  tags?: string[];
  priority?: string | null;
  topicIds?: string[];
}) =>
  invoke<CalEvent>("create_event", {
    subjectId: e.subjectId,
    title: e.title,
    description: e.description,
    location: e.location,
    color: e.color,
    startMs: e.startMs,
    endMs: e.endMs,
    allDay: e.allDay,
    kind: e.kind,
    reminderMs: e.reminderMs,
    tags: e.tags,
    priority: e.priority,
    topicIds: e.topicIds,
  });
export const listEvents = (
  subjectId?: string | null,
  fromMs?: number | null,
  toMs?: number | null
) => invoke<CalEvent[]>("list_events", { subjectId, fromMs, toMs });
export const updateEvent = (e: {
  id: string;
  title: string;
  startMs: number;
  description?: string | null;
  location?: string | null;
  color?: string | null;
  endMs?: number | null;
  allDay?: boolean;
  kind?: string;
  reminderMs?: number | null;
  tags?: string[];
  /** Omit to keep the stored value; "none" clears. */
  priority?: string | null;
  /** Omit to keep the stored value. */
  topicIds?: string[];
}) =>
  invoke<CalEvent>("update_event", {
    id: e.id,
    title: e.title,
    description: e.description,
    location: e.location,
    color: e.color,
    startMs: e.startMs,
    endMs: e.endMs,
    allDay: e.allDay,
    kind: e.kind,
    reminderMs: e.reminderMs,
    tags: e.tags,
    priority: e.priority ?? undefined,
    topicIds: e.topicIds,
  });
export const deleteEvent = (id: string) => invoke<void>("delete_event", { id });
export const setEventDone = (id: string, done: boolean) =>
  invoke<CalEvent>("set_event_done", { id, done });
export const setEventStatus = (id: string, status: "todo" | "doing" | "done") =>
  invoke<CalEvent>("set_event_status", { id, status });
/** Open an http(s) URL in the system browser (webview <a target=_blank> is a no-op in Tauri). */
export const openExternal = (url: string) => invoke<void>("open_external", { url });
/** Set the deadline study checklist (ticked topic ids). */
export const setEventChecklist = (id: string, topicIds: string[]) =>
  invoke<CalEvent>("set_event_checklist", { id, topicIds });
export const checkReminders = (systemNotify = false) =>
  invoke<CalEvent[]>("check_reminders", { systemNotify });

// ---- citations (per-subject bibliography) ----
export interface Reference {
  id: string;
  subjectId: string;
  ctype: string; // article | book | web | other
  title: string;
  authors: string | null;
  year: string | null;
  container: string | null;
  url: string | null;
  doi: string | null;
  notes: string | null;
  created_at: number;
  updated_at: number;
}
export interface CitationFields {
  ctype: string;
  title: string;
  authors?: string | null;
  year?: string | null;
  container?: string | null;
  url?: string | null;
  doi?: string | null;
  notes?: string | null;
}
export const addCitation = (subjectId: string, f: CitationFields) =>
  invoke<string>("add_citation", { subjectId, ...f });
export const listCitations = (subjectId: string) =>
  invoke<Reference[]>("list_citations", { subjectId });
export const updateCitation = (id: string, f: CitationFields) =>
  invoke<void>("update_citation", { id, ...f });
export const deleteCitation = (id: string) =>
  invoke<void>("delete_citation", { id });

// ---- review (spaced repetition over wrong answers) ----
export interface Attempt {
  id: string;
  subject_id: string;
  material_id: string | null;
  kind: string; // quiz | flashcard
  item_index: number;
  item_key: string;
  correct: boolean;
  created_at: number;
}
export interface ReviewItem {
  item_index: number;
  item_key: string;
}
export const recordAttempt = (
  subjectId: string,
  kind: "quiz" | "flashcard",
  itemIndex: number,
  itemKey: string,
  correct: boolean,
  materialId?: string | null
) =>
  invoke<void>("record_attempt", {
    subjectId,
    materialId,
    kind,
    itemIndex,
    itemKey,
    correct,
  });
export const reviewSet = (subjectId: string, kind: "quiz" | "flashcard") =>
  invoke<ReviewItem[]>("review_set", { subjectId, kind });

// ---- SM-2 spaced repetition ----
export interface DueCard {
  item_index: number;
  item_key: string;
  due_at: number;
  reps: number;
  interval_d: number;
}
export interface SrsResult {
  due_at: number;
  interval_d: number;
  reps: number;
  ease: number;
}
export interface SrsStats {
  due: number;
  total: number;
}
/** Grade a card with SM-2. quality 0-5 (Again≈1, Hard≈3, Good≈4, Easy≈5). */
export const srsGrade = (
  subjectId: string,
  kind: "quiz" | "flashcard",
  itemIndex: number,
  itemKey: string,
  quality: number,
  materialId?: string | null
) =>
  invoke<SrsResult>("srs_grade", {
    subjectId,
    materialId,
    kind,
    itemIndex,
    itemKey,
    quality,
  });
/** Cards due for review now (oldest-due first). */
export const srsDue = (subjectId: string, kind: "quiz" | "flashcard") =>
  invoke<DueCard[]>("srs_due", { subjectId, kind });
/** Next interval (days) per grade: [again, hard, good, easy]. */
export const srsPreview = (subjectId: string, kind: "quiz" | "flashcard", itemKey: string) =>
  invoke<number[]>("srs_preview", { subjectId, kind, itemKey });
/** Due-now + total scheduled counts for a subject+kind. */
export const srsStats = (subjectId: string, kind: "quiz" | "flashcard") =>
  invoke<SrsStats>("srs_stats", { subjectId, kind });
/** One card in the cross-subject "review today" queue. */
export interface ReviewTodayCard {
  subject_id: string;
  subject_name: string;
  kind: string; // quiz | flashcard
  material_id: string | null;
  item_index: number;
  item_key: string;
  due_at: number;
  reps: number;
  interval_d: number;
  stability: number | null;
  difficulty: number | null;
}
/** Every card due right now across all subjects, oldest-due first. */
export const reviewToday = (limit?: number) =>
  invoke<ReviewTodayCard[]>("review_today", { limit });

// ---- study analytics ----
export interface DayMinutes { day: string; minutes: number }
export interface DayReviews { day: string; reviews: number; correct: number; accuracy: number }
export interface DueDay { day: string; due: number }
export interface SubjectStat {
  subject_id: string;
  minutes: number;
  reviews: number;
  correct: number;
  accuracy: number;
}
export interface FsrsTotals { cards: number; avg_stability: number; lapses: number }
export interface WeakTopic {
  subject_id: string;
  topic_id: string;
  topic_name: string;
  reviews: number;
  correct: number;
  accuracy: number;
  lapses: number;
  avg_stability: number;
  reason: string;
}
export interface PomodoroStats {
  focus_sessions: number;
  focus_minutes: number;
  break_minutes: number;
  avg_session_min: number;
  longest_session_min: number;
  by_hour: number[];
}
export interface AnalyticsSummary {
  minutes_per_day: DayMinutes[];
  /** A full rolling year (366 days) of daily study minutes for the heatmap. */
  year_minutes: DayMinutes[];
  reviews_per_day: DayReviews[];
  due_forecast: DueDay[];
  per_subject: SubjectStat[];
  weak_topics: WeakTopic[];
  fsrs: FsrsTotals;
  streak: number;
  /** Longest run of consecutive days with activity, anywhere in the year. */
  longest_streak: number;
  pomodoro: PomodoroStats;
  // B2: work-items habit surface (counters day-by-day in local time).
  work_streak: number;
  /** Longest past run of consecutive days with a done work item in the year. */
  work_longest_streak: number;
  /** Items completed this calendar week (Mon..today, local). */
  work_week_done: number;
  /** Items NOT dropped this calendar week - denominator of the completion rate. */
  work_week_total: number;
  minutes_week: number;
  reviews_week: number;
  accuracy_week: number;
}
/** Log a study segment: "work"/"break" (pomodoro) or "app" (passive focused time). */
export const logPomodoroSession = (
  subjectId: string | null,
  kind: "work" | "break" | "app",
  startedMs: number,
  endedMs: number
) => invoke<void>("log_pomodoro_session", { subjectId, kind, startedMs, endedMs });
/** One finished focus/break span for the time-block timeline. */
export interface PomodoroSegment {
  id: string;
  subject_id: string | null;
  subject_name: string | null;
  kind: string; // work | break
  started_ms: number;
  ended_ms: number;
}
/** Today's finished focus/break segments (default [local midnight, now)). */
export const pomodoroSegments = (sinceMs?: number, untilMs?: number) =>
  invoke<PomodoroSegment[]>("pomodoro_segments", { sinceMs, untilMs });
/** The whole Study Analytics dashboard in one call (default 30-day window). */
export const analyticsSummary = (days?: number) =>
  invoke<AnalyticsSummary>("analytics_summary", { days });
export interface TopicStat {
  topic_id: string; topic_name: string; reviews: number; correct: number;
  accuracy: number; lapses: number; cards: number; avg_stability: number;
  sources: number; materials: number;
}
export const topicStats = (subjectId: string, days?: number) =>
  invoke<TopicStat[]>("topic_stats", { subjectId, days });

// ---- events ----
export const onIngestProgress = (
  cb: (p: IngestProgress) => void
): Promise<UnlistenFn> => listen<IngestProgress>("ingest:progress", (e) => cb(e.payload));
/** Fired by the tray menu's "Play / pause music" item. */
export const onTrayMusicToggle = (cb: () => void): Promise<UnlistenFn> =>
  listen("tray-music-toggle", () => cb());
export const onTrayGoDashboard = (cb: () => void): Promise<UnlistenFn> =>
  listen("tray-go-dashboard", () => cb());
/** Fired when a background job creates a note (e.g. auto lecture summary). */
export const onNoteCreated = (cb: () => void): Promise<UnlistenFn> =>
  listen("note:created", () => cb());
/** Fired after live sync merges peers' changes into the local DB. */
export const onSyncApplied = (cb: () => void): Promise<UnlistenFn> =>
  listen("sync:applied", () => cb());

// ---- homelab per-service status (Settings → Integrations) ----
export interface HomelabServiceStatus {
  id: string;
  label: string;
  configured: boolean;
  ok: boolean;
  detail: string;
}
/** Probe every homelab service through the URLs the app actually uses. */
export const homelabStatus = () => invoke<HomelabServiceStatus[]>("homelab_status");

// ---- notification tap deep links (mobile) ----
// Where a tapped OS notification should land in the app; stored by the backend
// (alerts.rs) keyed on the notification's numeric id.
export interface NotifRoute {
  id: number;
  /** "lecture" (→ subject) · "deadline"/"exam"/"event" (→ calendar day) */
  kind: string;
  subjectId?: string | null;
  ts?: number | null;
}
export const notificationRoute = (id: number) =>
  invoke<NotifRoute | null>("notification_route", { id });

/** Listen for OS-notification taps (mobile only — the desktop plugin has no
 *  tap events). The callback gets the tapped notification's numeric id. */
export async function onNotificationTap(cb: (id: number) => void): Promise<void> {
  const { addPluginListener } = await import("@tauri-apps/api/core");
  await addPluginListener(
    "notification",
    "actionPerformed",
    (data: { actionId?: string; notification?: { id?: number } }) => {
      // "tap" is the plain open action; explicit action buttons pass through too.
      if (data?.actionId === "dismiss") return;
      const id = data?.notification?.id;
      if (typeof id === "number") cb(id);
    }
  );
}

// ---- knowledge framework + calibration + undo journal -----------------------

export interface KnowledgeNode {
  id: string;
  subject_id: string;
  topic_id?: string | null;
  source_id?: string | null;
  parent_id?: string | null;
  node_key: string;
  kind: string;          // subject | topic | source | concept
  title: string;
  summary?: string | null;
  position: number;
  coverage: number;      // 0..1
  mastery: number;       // 0..1
  importance: number;    // 0..1
  confidence: number;    // 0..1
  observations: number;
  /** How many of your own notes mention this point (coverage counts these). */
  note_count: number;
  created_at: number;
  updated_at: number;
}

export interface KnowledgeEdge {
  id: string;
  subject_id: string;
  from_id: string;
  to_id: string;
  kind: string;          // contains | related | backlink
  weight: number;
  created_at: number;
}

export interface KnowledgeGraph {
  subject_id: string;
  nodes: KnowledgeNode[];
  edges: KnowledgeEdge[];
}

export interface CalibrationBucket {
  label: string;             // "0–20%" | "20–40%" | ...
  observations: number;
  correct: number;
  mean_confidence: number;   // 0..1
  accuracy: number;          // 0..1
  /** accuracy - mean_confidence; positive => underconfident, negative => overconfident. */
  gap: number;
}

export interface CalibrationSummary {
  observations: number;
  correct: number;
  mean_confidence: number;
  accuracy: number;
  brier_score: number;
  buckets: CalibrationBucket[];
}

export interface UndoEntry {
  id: string;
  action: string;           // delete_subject | delete_topic | delete_source | ...
  entity_table: string;     // subject | topic | source | note | material
  entity_id: string;
  label: string;
  payload: Record<string, unknown>;
  created_at: number;
  expires_at: number;
  undone_at?: number | null;
}

export const syncKnowledgeGraph = (subjectId: string) =>
  invoke<number>("sync_knowledge_graph", { subjectId });

export const listKnowledgeNodes = (subjectId: string) =>
  invoke<KnowledgeNode[]>("list_knowledge_nodes", { subjectId });

/** Re-scan notes → knowledge nodes (after an upgrade, or if the tree looks
 *  stale). Omit the subject to rebuild every note in the library. */
export const noteKnowledgeResync = (subjectId?: string | null) =>
  invoke<number>("note_knowledge_resync", { subjectId: subjectId ?? null });

/** 综述 → MOC: a map of content for one subject. The model writes the prose;
 *  `gaps` counts knowledge points the coverage index says are still thin. */
export interface MocOutcome {
  note_id: string;
  title: string;
  /** Relative path under the vault's `moc/` folder, when a vault is set. */
  vault_path: string | null;
  notes: number;
  gaps: number;
  /** False when the deterministic composer was used (no model / unusable reply). */
  used_model: boolean;
}
export const mocGenerate = (subjectId: string, title?: string | null) =>
  invoke<MocOutcome>("moc_generate", { subjectId, title: title ?? null });

export const getKnowledgeGraph = (subjectId: string) =>
  invoke<KnowledgeGraph>("get_knowledge_graph", { subjectId });

// ---- knowledge point docs — local layers, model prose ----------------------

export interface NodeLayer {
  node_id: string;
  title: string;
  kind: string;
  layer: number;
}

export interface NodeDoc {
  node_id: string;
  node_title: string;
  subject_id: string;
  /** This node's layer in the local prerequisite peel (0 = foundation). */
  layer: number;
  layers: NodeLayer[];
  prereqs: string[];
  has_doc: boolean;
  body: string;
  why: string;
  model: string;
  updated_at: number;
}

export const nodeDocGet = (nodeId: string) =>
  invoke<NodeDoc>("node_doc_get", { nodeId });

export const nodeDocGenerate = (nodeId: string) =>
  invoke<NodeDoc>("node_doc_generate", { nodeId });

export const nodeDocDelete = (nodeId: string) =>
  invoke<void>("node_doc_delete", { nodeId });

// ---- source view — the materials × points bipartite, with 孤证 called out --

export interface ProvenanceMaterial {
  id: string;
  name: string;
  /** "source" | "note" */
  kind: string;
  /** For sources: whether it parsed into chunks (null for notes). */
  parsed: boolean | null;
  /** How many points this material supports. */
  supports: number;
}

export interface ProvenancePoint {
  id: string;
  title: string;
  /** "topic" | "concept" */
  kind: string;
  source_count: number;
  note_count: number;
  /** cross | single | notes_only | orphan */
  verdict: string;
}

export interface ProvenanceEdge {
  material_id: string;
  point_id: string;
  hits: number;
  /** "topic" | "text" | "mention" */
  via: string;
}

export interface Bipartite {
  materials: ProvenanceMaterial[];
  points: ProvenancePoint[];
  edges: ProvenanceEdge[];
}

export const provenanceBipartite = (subjectId: string) =>
  invoke<Bipartite>("provenance_bipartite", { subjectId });

// ---- tutor bench — a scaffold ladder that moves both ways ------------------

export interface TutorSession {
  id: string;
  subject_id: string;
  subject_name: string;
  node_id: string | null;
  node_title: string | null;
  title: string;
  level: number;
  level_label: string;
  turns: number;
  status: string;
  updated_at: number;
}

export interface TutorTurn {
  id: string;
  role: "user" | "tutor";
  text: string;
  level: number;
  confidence: number | null;
  verdict: string | null;
  created_at: number;
}

export interface TutorExchange {
  session_id: string;
  verdict: string;
  reply: string;
  level: number;
  level_label: string;
  turns: number;
  /** True when this turn also opened a next-day calibration slot. */
  misjudged: boolean;
}

export interface TutorOpened {
  session: TutorSession;
  opening: string;
}

export interface TutorMisjudged {
  id: string;
  session_id: string;
  subject_id: string;
  node_id: string | null;
  node_title: string | null;
  question: string;
  answer: string;
  scheduled_for: number;
  due: boolean;
  created_at: number;
}

export const tutorStart = (subjectId: string, nodeId: string | null) =>
  invoke<TutorOpened>("tutor_start", { subjectId, nodeId: nodeId ?? null });

export const tutorReply = (sessionId: string, answer: string, confidence: number) =>
  invoke<TutorExchange>("tutor_reply", { sessionId, answer, confidence });

export const tutorSessions = (subjectId: string) =>
  invoke<TutorSession[]>("tutor_sessions", { subjectId });

export const tutorHistory = (sessionId: string) =>
  invoke<TutorTurn[]>("tutor_history", { sessionId });

export const tutorEnd = (sessionId: string) =>
  invoke<void>("tutor_end", { sessionId });

export const tutorMisjudged = () =>
  invoke<TutorMisjudged[]>("tutor_misjudged");

export const tutorMisjudgedResolve = (id: string) =>
  invoke<void>("tutor_misjudged_resolve", { id });

export const tutorDueCount = () =>
  invoke<number>("tutor_due_count");

export const applyObservation = (
  nodeId: string,
  correct: boolean,
  confidence: number,
  signal?: string,
  note?: string,
) =>
  invoke<number>("apply_observation", {
    nodeId,
    correct,
    confidence,
    signal: signal ?? null,
    note: note ?? null,
  });

export const calibrationSummary = (subjectId: string) =>
  invoke<CalibrationSummary>("calibration_summary", { subjectId });

// ---- adaptive sprint planner -----------------------------------------

export interface SprintTopic {
  topic_id: string;
  topic_name: string;
  /** Effective mastery 0..1 (BKT blended with coverage + recent accuracy). */
  mastery: number;
  coverage: number;
  recent_accuracy: number;
  reviews: number;
  /** Unmet mastery relative to the plan's target (0..1). */
  gap: number;
  /** Share of the total remaining gap, 0..1. */
  weight: number;
  rank: number;
  minutes: number;
  /** critical | focus | solid */
  status: string;
}

export interface SprintPlan {
  subject_id: string;
  subject_name: string;
  target_score: number;
  current_score: number;
  mastery_now: number;
  mastery_target: number;
  days_left: number;
  daily_minutes: number;
  minutes_required: number;
  per_day_minutes: number;
  minutes_available: number;
  feasible: boolean;
  topics: SprintTopic[];
  notes: string[];
}

/**
 * Goal-driven study plan for a subject. `deadlineMs` is an absolute timestamp;
 * `dailyMinutes` falls back to the `daily_study_minutes` setting on the backend.
 */
export const sprintPlan = (
  subjectId: string,
  targetScore: number,
  deadlineMs: number,
  dailyMinutes?: number,
) =>
  invoke<SprintPlan>("sprint_plan", {
    subjectId,
    targetScore,
    deadlineMs,
    dailyMinutes: dailyMinutes ?? null,
  });

export const listUndoEntries = (limit = 50) =>
  invoke<UndoEntry[]>("list_undo_entries", { limit });

export const restoreUndo = (id: string) =>
  invoke<string>("restore_undo", { id });

export const exportObsidian = (directory: string) => invoke<string>("export_obsidian", { directory });

// ---- work items: the cross-subject spine (migration 0037) ----------------
//
// Unlike everything above, these are NOT scoped to a subject — `subject_id`
// is optional, which is what lets the same table hold a daily errand, a study
// task, an exam milestone and a paper to read.

export type ItemStatus = "inbox" | "open" | "doing" | "blocked" | "done" | "dropped";
export type ItemKind =
  | "task" | "reading" | "writing" | "question" | "excerpt" | "milestone" | "material";
export type ItemPriority = "low" | "med" | "high" | "urgent";

export interface WorkItem {
  id: string;
  kind: ItemKind | string;
  title: string;
  body: string | null;
  status: ItemStatus | string;
  priority: ItemPriority | string | null;
  due_ms: number | null;
  start_ms: number | null;
  estimate_min: number | null;
  actual_min: number;
  parent_id: string | null;
  rrule: string | null;
  subject_id: string | null;
  topic_id: string | null;
  goal_id: string | null;
  sort_order: number;
  created_at: number;
  updated_at: number;
  done_at: number | null;
  child_count: number;
  open_child_count: number;
  next_due_ms: number | null;
}

export interface ItemInput {
  kind?: string;
  title: string;
  body?: string | null;
  status?: string;
  priority?: string | null;
  due_ms?: number | null;
  start_ms?: number | null;
  estimate_min?: number | null;
  parent_id?: string | null;
  rrule?: string | null;
  subject_id?: string | null;
  topic_id?: string | null;
  goal_id?: string | null;
}

export interface ItemQuery {
  status?: string;          // "inbox" | "active" | a concrete status | "all"
  kind?: string;
  subject_id?: string;
  goal_id?: string;
  parent_id?: string;
  due_before_ms?: number;
  search?: string;
  limit?: number;
}

export interface ItemLink {
  item_id: string;
  entity_table: string;
  entity_id: string;
  role: string;
  created_at: number;
}

export interface Goal {
  id: string;
  title: string;
  kind: string;
  metric: string | null;
  target: number | null;
  current: number;
  deadline_ms: number | null;
  subject_id: string | null;
  status: string;
  created_at: number;
  updated_at: number;
  item_count: number;
  done_item_count: number;
}

export interface GoalInput {
  id?: string | null;
  title: string;
  kind?: string;
  metric?: string | null;
  target?: number | null;
  current?: number | null;
  deadline_ms?: number | null;
  subject_id?: string | null;
  status?: string;
}

export interface PlanBlock {
  id: string;
  item_id: string;
  day: string;
  start_min: number;
  end_min: number;
  status: string;
  origin: string;
  created_at: number;
  updated_at: number;
}

export interface TodayCounts {
  overdue: number;
  due_today: number;
  inbox: number;
  done_today: number;
  open: number;
  planned_minutes: number;
  done_minutes: number;
}

export interface TodayBoard {
  overdue: WorkItem[];
  today: WorkItem[];
  inbox: WorkItem[];
  blocks: PlanBlock[];
  counts: TodayCounts;
}

export const listWorkItems = (query?: ItemQuery) =>
  invoke<WorkItem[]>("list_work_items", { query: query ?? null });
export const getWorkItem = (id: string) => invoke<WorkItem>("get_work_item", { id });
export const createWorkItem = (input: ItemInput) =>
  invoke<WorkItem>("create_work_item", { input });
export const updateWorkItem = (id: string, input: ItemInput) =>
  invoke<WorkItem>("update_work_item", { id, input });
export const setWorkItemStatus = (id: string, status: string) =>
  invoke<WorkItem>("set_work_item_status", { id, status });
export const addWorkItemMinutes = (id: string, minutes: number) =>
  invoke<WorkItem>("add_work_item_minutes", { id, minutes });
export const deleteWorkItem = (id: string) => invoke<void>("delete_work_item", { id });
export const linkWorkItem = (
  itemId: string,
  entityTable: string,
  entityId: string,
  role?: string,
) =>
  invoke<void>("link_work_item", {
    itemId,
    entityTable,
    entityId,
    role: role ?? null,
  });
export const unlinkWorkItem = (itemId: string, entityTable: string, entityId: string) =>
  invoke<void>("unlink_work_item", { itemId, entityTable, entityId });
export const listWorkItemLinks = (itemId: string) =>
  invoke<ItemLink[]>("list_work_item_links", { itemId });
export const workItemsForEntity = (entityTable: string, entityId: string) =>
  invoke<WorkItem[]>("work_items_for_entity", { entityTable, entityId });

export const listGoals = () => invoke<Goal[]>("list_goals");
export const upsertGoal = (input: GoalInput) => invoke<Goal>("upsert_goal", { input });
export const deleteGoal = (id: string) => invoke<void>("delete_goal", { id });

/** Local-midnight boundaries are computed by the caller (see `localDay` in Today.svelte). */
export const todayBoard = (dayStartMs: number, dayEndMs: number, day: string) =>
  invoke<TodayBoard>("today_board", { dayStartMs, dayEndMs, day });

export const listPlanBlocks = (day: string) =>
  invoke<PlanBlock[]>("list_plan_blocks", { day });
export const upsertPlanBlock = (
  itemId: string,
  day: string,
  startMin: number,
  endMin: number,
  id?: string,
  status?: string,
) =>
  invoke<PlanBlock>("upsert_plan_block", {
    id: id ?? null,
    itemId,
    day,
    startMin,
    endMin,
    status: status ?? null,
  });
export const deletePlanBlock = (id: string) => invoke<void>("delete_plan_block", { id });

// ---- planner: open items -> concrete plan blocks -------------------------

export interface PlanRequest {
  start_day: string;          // local YYYY-MM-DD
  days?: number | null;
  daily_minutes?: number | null;
  day_start_min?: number | null;
  day_end_min?: number | null;
  break_min?: number | null;
  replace?: boolean | null;
}

export interface Unscheduled {
  item_id: string;
  title: string;
  minutes: number;
  reason: string;
}

export interface PlanOutcome {
  start_day: string;
  days: number;
  blocks: PlanBlock[];
  scheduled_items: number;
  unscheduled: Unscheduled[];
  minutes_planned: number;
  minutes_capacity: number;
  notes: string[];
}

export interface RolloverOutcome {
  moved: number;
  from_days: string[];
}

export interface BlockRow extends PlanBlock {
  title: string;
  item_status: string;
  kind: string;
  estimate_min: number | null;
}

export const planWeek = (req: PlanRequest) => invoke<PlanOutcome>("plan_week", { req });
export const planRollover = (today: string) =>
  invoke<RolloverOutcome>("plan_rollover", { today });
export const completePlanBlock = (id: string) =>
  invoke<PlanBlock>("complete_plan_block", { id });
export const planBoard = (startDay: string, days: number) =>
  invoke<BlockRow[]>("plan_board", { startDay, days });

export interface GoalPlanRequest {
  goal_id: string;
  start_day: string;
  daily_minutes?: number | null;
}

export interface GoalPlanOutcome {
  goal_id: string;
  created: number;
  skipped: number;
  minutes: number;
  titles: string[];
  notes: string[];
}

export const materializeGoal = (req: GoalPlanRequest) =>
  invoke<GoalPlanOutcome>("materialize_goal", { req });

// ---- papers: the research reading list (migration 0038) ------------------

export interface Paper {
  id: string;
  title: string;
  authors: string | null;
  year: number | null;
  venue: string | null;
  doi: string | null;
  url: string | null;
  abstract_text: string | null;
  status: string; // to_read | reading | read | cited | dropped
  rating: number | null;
  notes: string | null;
  subject_id: string | null;
  source_id: string | null;
  added_at: number;
  updated_at: number;
  /** Stable citation key assigned on import (`Bishop2025Attention`). */
  cite_key: string | null;
  excerpt_count: number;
  open_task_count: number;
}

export interface Excerpt {
  id: string;
  paper_id: string | null;
  source_id: string | null;
  quote: string;
  page: number | null;
  note: string | null;
  tags: string | null;
  created_at: number;
  updated_at: number;
  paper_title: string | null;
  /** PDF highlight rects: JSON [{x,y,w,h}] in page-fraction coordinates. */
  rects: string | null;
}

export interface PaperInput {
  title: string;
  authors?: string | null;
  year?: number | null;
  venue?: string | null;
  doi?: string | null;
  url?: string | null;
  abstract_text?: string | null;
  subject_id?: string | null;
  source_id?: string | null;
}

export interface PaperPatch {
  status?: string | null;
  rating?: number | null;
  notes?: string | null;
  subject_id?: string | null;
  source_id?: string | null;
}

export interface BibImportOutcome {
  imported: number;
  skipped: number;
  titles: string[];
}

export const paperImport = (input: PaperInput) => invoke<Paper>("paper_import", { input });
export const paperList = (status?: string, subjectId?: string, search?: string) =>
  invoke<Paper[]>("paper_list", {
    status: status ?? null,
    subjectId: subjectId ?? null,
    search: search ?? null,
  });
export const paperGet = (id: string) => invoke<Paper>("paper_get", { id });
export const paperUpdate = (id: string, patch: PaperPatch) =>
  invoke<Paper>("paper_update", { id, patch });
export const paperDelete = (id: string) => invoke<void>("paper_delete", { id });
export const paperStartReading = (paperId: string, dueMs?: number) =>
  invoke<WorkItem>("paper_start_reading", { paperId, dueMs: dueMs ?? null });
export const excerptAdd = (
  quote: string,
  paperId?: string,
  sourceId?: string,
  page?: number,
  note?: string,
  tags?: string,
  rects?: string,
) =>
  invoke<Excerpt>("excerpt_add", {
    paperId: paperId ?? null,
    sourceId: sourceId ?? null,
    quote,
    page: page ?? null,
    note: note ?? null,
    tags: tags ?? null,
    rects: rects ?? null,
  });
/** Highlights taken on one ingested source (PDF reader sidebar + overlays). */
export const excerptsListSource = (sourceId: string) =>
  invoke<Excerpt[]>("excerpts_list_source", { sourceId });
export const excerptList = (paperId?: string) =>
  invoke<Excerpt[]>("excerpt_list", { paperId: paperId ?? null });
export const excerptDelete = (id: string) => invoke<void>("excerpt_delete", { id });

export const paperBibtex = (ids?: string[]) =>
  invoke<string>("paper_bibtex", { ids: ids ?? null });
export const paperImportBibtex = (text: string) =>
  invoke<BibImportOutcome>("paper_import_bibtex", { text });

// ---- B4: cross-paper matrix -----------------------------------------------

export interface MatrixRow {
  label: string;
  cells: string[];
}
export interface PaperMatrix {
  columns: string[];       // paper titles, in selection order
  rows: MatrixRow[];        // one entry per row label, aligned with columns
  generated_by: string;     // model name (or "local cache")
  generated_at: number;
}

/** Build a cross-paper comparison matrix. `paperIds` is the user selection;
 *  `rowLabels` overrides the default four rows; empty list falls back. */
export const paperBuildMatrix = (paperIds: string[], rowLabels?: string[]) =>
  invoke<PaperMatrix>("paper_build_matrix", {
    paperIds,
    rowLabels: rowLabels ?? null,
  });

// ---- B5: excerpts → citable draft ------------------------------------------

export interface DraftReference {
  n: number;
  excerptId: string;
  paperId: string | null;
  title: string;
  authors: string | null;
  year: number | null;
  page: number | null;
}
export interface DraftParagraph {
  text: string;
  cites: number[];
}
export interface DraftSection {
  heading: string;
  paragraphs: DraftParagraph[];
}
export interface DraftPayload {
  thesis: string;
  /** Legacy flat drafts (pre-sections). */
  paragraphs?: DraftParagraph[];
  /** Multi-section drafts (D-写): rendered as real headings everywhere. */
  sections?: DraftSection[];
  references: DraftReference[];
}
export interface ResearchDraft {
  id: string;
  title: string;
  excerpt_ids: string[];
  meta: string | null;
  payload: DraftPayload | null;
  created_at: number;
}

/** Turn picked excerpts (in citation order) into a citable passage. [n] ==
 *  excerptIds[n-1]; the reference list is built server-side from the excerpts. */
export const generateDraft = (excerptIds: string[], title?: string, customPrompt?: string) =>
  invoke<ResearchDraft>("generate_draft", {
    excerptIds,
    title: title ?? null,
    customPrompt: customPrompt ?? null,
  });
export const draftList = () => invoke<ResearchDraft[]>("draft_list");
export const draftDelete = (id: string) => invoke<void>("draft_delete", { id });

// ---- 2.1 写作升格批改 (draft review) ----------------------------------------

export interface DraftFinding {
  kind: "hedge" | "colloquial" | "cliche" | "first_person" | "long_sentence" | "long_para" | string;
  severity: "info" | "warn";
  /** null = legacy flat paragraph */
  section: number | null;
  para: number;
  sentence: number | null;
  excerpt: string;
  hint: string;
}
/** Sentences per paragraph, split server-side — the SAME indices the rewrite
 *  commands address, so a UI click always rewrites exactly that sentence. */
export interface DraftText {
  lang: string;
  sections: string[][][];
  flat: string[][];
  findings: DraftFinding[];
}
export interface DraftUpgrade {
  section: number | null;
  para: number;
  sentence: number;
  original: string;
  revised: string;
  reason: string;
}
export interface DraftUpgradeReport {
  lang: string;
  overall: string;
  suggestions: DraftUpgrade[];
}
export interface SentenceRewrite {
  original: string;
  revised: string;
  reason: string;
}

/** 规则体检 — instant, no LLM. Also returns the sentence tables. */
export const draftReviewCheck = (draftId: string) =>
  invoke<DraftText>("draft_review_check", { draftId });
/** AI 升格 — one call, whole draft, before→after suggestions. */
export const draftReviewUpgrade = (draftId: string) =>
  invoke<DraftUpgradeReport>("draft_review_upgrade", { draftId });
/** 逐句改写 — one focused call; nothing persisted until draftUpdate. */
export const draftRewriteSentence = (
  draftId: string,
  section: number | null,
  para: number,
  sentence: number,
  instruction?: string,
) =>
  invoke<SentenceRewrite>("draft_rewrite_sentence", {
    draftId,
    section,
    para,
    sentence,
    instruction: instruction ?? null,
  });
/** Persist an edited draft payload (accepted rewrites patch sentences). */
export const draftUpdate = (draftId: string, payload: DraftPayload) =>
  invoke<void>("draft_update", { draftId, payload });

// ---- writing toolbox + the upgrade ledger (0071) ---------------------------

export interface KitEntry {
  id: string;
  category: string;
  title: string;
  content: string;
  /** builtin | user | harvest */
  source: string;
  created_at: number;
  updated_at: number;
}
export interface UpgradeRecord {
  id: string;
  draft_id: string;
  /** upgrade | rewrite */
  kind: string;
  before_text: string;
  after_text: string;
  reason: string | null;
  accepted: boolean;
  created_at: number;
}
export const writingKitList = (category?: string) =>
  invoke<KitEntry[]>("writing_kit_list", { category: category ?? null });
export const writingKitSave = (category: string, title: string, content: string) =>
  invoke<string>("writing_kit_save", { category, title, content });
export const writingKitDelete = (id: string) =>
  invoke<void>("writing_kit_delete", { id });
export const upgradeHistoryList = (draftId: string) =>
  invoke<UpgradeRecord[]>("upgrade_history_list", { draftId });
/** Record an acted-on suggestion; an accepted one is harvested into the toolbox. */
export const upgradeOutcomeRecord = (
  draftId: string,
  kind: "upgrade" | "rewrite",
  before: string,
  after: string,
  reason: string | null,
  accepted: boolean,
) =>
  invoke<string | null>("upgrade_outcome_record", {
    draftId,
    kind,
    before,
    after,
    reason,
    accepted,
  });

// ---- english bench — close reading (six ways) + translation training -------

export interface BenchRun {
  id: string;
  /** close | translate */
  kind: string;
  source_id: string;
  source_name: string;
  status: string;
  error: string | null;
  model: string | null;
  truncated: boolean;
  created_at: number;
  updated_at: number;
}
export interface BenchItem {
  id: string;
  dimension: string;
  ord: number;
  /** pending | done | error */
  status: string;
  error: string | null;
  payload: unknown | null;
  created_at: number;
}
export interface BenchResult {
  run: BenchRun;
  items: BenchItem[];
}
export const benchRunCreate = (kind: "close" | "translate", sourceId: string) =>
  invoke<string>("bench_run_create", { kind, sourceId });
export const benchRunGet = (kind: "close" | "translate", sourceId: string) =>
  invoke<BenchResult | null>("bench_run_get", { kind, sourceId });
export const benchRunDelete = (runId: string) =>
  invoke<void>("bench_run_delete", { runId });
export const benchRunExecute = (runId: string) =>
  invoke<void>("bench_run_execute", { runId });
export const benchItemRetry = (runId: string, dimension: string) =>
  invoke<void>("bench_item_retry", { runId, dimension });
export const benchExportMd = (runId: string) =>
  invoke<string>("bench_export_md", { runId });
export const benchMdFile = (runId: string, dest: string) =>
  invoke<void>("bench_md_file", { runId, dest });
/** Push one synonym pair into the subject's vocab deck (enters the SRS queue). */
export const benchSynonymSave = (
  sourceId: string,
  original: string,
  upgrade: string,
  context: string,
) =>
  invoke<void>("bench_synonym_save", { sourceId, original, upgrade, context });

// ---- office export + edit-back (A2/A3) --------------------------------------

/** Material → Word (.docx) at `dest`, rendered from the stored payload. */
export const exportMaterialDocx = (materialId: string, dest: string) =>
  invoke<void>("export_material_docx", { materialId, dest });
/** Material → Excel (.xlsx) with formulas + charts where the kind supports them. */
export const exportMaterialXlsx = (materialId: string, dest: string) =>
  invoke<void>("export_material_xlsx", { materialId, dest });
/** Material → PowerPoint (.pptx) deck (cover + content slides). */
export const exportMaterialPptx = (materialId: string, dest: string) =>
  invoke<void>("export_material_pptx", { materialId, dest });
/** Research draft → Word (.docx) with citation numbers and the reference table. */
export const exportDraftDocx = (draftId: string, dest: string) =>
  invoke<void>("export_draft_docx", { draftId, dest });
/** Export a draft as a COMPILABLE .tex (ctexart; xelatex for Chinese). */
export const exportDraftTex = (draftId: string, dest: string) =>
  invoke<void>("export_draft_tex", { draftId, dest });

/** Visible paragraph texts of an existing .docx (edit anchors match these). */
export const docxReadParagraphs = (path: string) =>
  invoke<string[]>("docx_read_paragraphs", { path });
export interface DocxEdit {
  anchor: string;
  op: "insert_after" | "comment";
  text: string;
  comment: string;
}
export interface DocxEditOutcome {
  applied: number;
  skipped: string[];
  output: string;
}
/** Write edits into an existing .docx as real Word tracked changes + comments
 *  (always a NEW file at `output`). Returns applied/skipped counts. */
export const docxApplyEdits = (path: string, output: string, author: string, edits: DocxEdit[]) =>
  invoke<DocxEditOutcome>("docx_apply_edits", { path, output, author, edits });
/** AI review: paragraphs → a previewed set of tracked-change edits. */
export const docxReviewEdits = (path: string, instruction?: string) =>
  invoke<DocxEdit[]>("docx_review_edits", { path, instruction: instruction ?? null });

/** Concatenate PDFs, in order, into one file at `dest`. */
export const pdfMerge = (paths: string[], dest: string) =>
  invoke<void>("pdf_merge", { paths, dest });
/** Split a PDF into one single-page file per page; returns the new file paths. */
export const pdfSplit = (path: string, outDir: string, stem: string) =>
  invoke<string[]>("pdf_split", { path, outDir, stem });
/** Stamp a diagonal gray text watermark on every page (new file at `dest`). */
export const pdfWatermark = (path: string, text: string, dest: string) =>
  invoke<void>("pdf_watermark", { path, text, dest });
export interface PdfField {
  name: string;
  kind: string;
  value: string;
  options: string[];
}
/** Enumerate AcroForm fields (text/checkbox/radio/choice) with current values. */
export const pdfFormFields = (path: string) =>
  invoke<PdfField[]>("pdf_form_fields", { path });
/** Fill AcroForm fields; returns how many were actually set. */
export const pdfFillForm = (path: string, values: [string, string][], dest: string) =>
  invoke<number>("pdf_fill_form", { path, values, dest });

// ---- plan vs actual (Insights) -------------------------------------------

export interface PlanDayStat {
  day: string;
  planned_min: number;
  done_min: number;
  planned_blocks: number;
  done_blocks: number;
}

export interface PlanVsActual {
  days: PlanDayStat[];
  planned_min: number;
  done_min: number;
  adherence: number;
  estimate_min: number;
  actual_min: number;
}

export const planVsActual = (startDay: string, days: number) =>
  invoke<PlanVsActual>("plan_vs_actual", { startDay, days });

// ---- retrieval evaluation -------------------------------------------------

export interface EvalRow {
  query: string;
  expect: string;
  got: string[];
  rank: number | null;
  /** Stratum: factual / conceptual / unanswerable / real. */
  kind: string;
  /** Score of the top-1 hit — the abstention signal. */
  top1: number;
}

/** One stratum's slice of a report. */
export interface Stratum {
  kind: string;
  cases: number;
  recall_at_k: number;
  mrr: number;
  mean_top1: number;
}

/** Answerable-vs-unanswerable score separation. When this collapses,
 *  retrieval can no longer tell "not in your library" from "in it". */
export interface Separation {
  answerable_mean_top1: number;
  unanswerable_mean_top1: number;
  /** Scale-free gap: (answerable − unanswerable) / answerable. */
  ratio: number;
}

export interface EvalReport {
  mode: string;
  k: number;
  evaluated: number;
  recall_at_k: number;
  mrr: number;
  rows: EvalRow[];
  /** Per-stratum breakdown (same metrics, sliced by case kind). */
  strata: Stratum[];
  /** Score separation, when the run has both answerable and unanswerable cases. */
  separation?: Separation | null;
}

/** One metric's verdict: adopt (improved with headroom) / regress (worse
 *  beyond tolerance) / neutral (inside the noise band). */
export interface MetricVerdict {
  metric: string;
  baseline: number;
  candidate: number;
  delta: number;
  verdict: "adopt" | "regress" | "neutral";
}

/** The mechanical verdict of one candidate run against the previous run of
 *  the same kind (KnowNote adoption rule). */
export interface EvalVerdict {
  baseline_at: number;
  mode: string;
  metrics: MetricVerdict[];
  /** adopt | regress | neutral */
  overall: "adopt" | "regress" | "neutral";
  note: string;
}

/** A run's result: the per-mode table plus the adoption verdict against the
 *  previous run, when there was one. */
export interface EvalOutcome {
  reports: EvalReport[];
  verdict: EvalVerdict | null;
}

/** Run the synthetic-corpus retrieval evaluation for all four modes. */
export const retrievalEval = (k?: number) =>
  invoke<EvalOutcome>("retrieval_eval", { k: k ?? null });

/** Run the same four modes over the user's OWN corpus. `subjectIds` scopes the
 *  pool (null/empty = every subject except the synthetic eval one). Queries are
 *  keyword terms sampled from indexed chunks; ground truth = the chunk's source. */
export const retrievalEvalReal = (subjectIds: string[] | null, k?: number) =>
  invoke<EvalOutcome>("retrieval_eval_real", { subjectIds, k: k ?? null });

// ---- planner estimate calibration ----------------------------------------

export interface KindFactor {
  kind: string;
  factor: number;
  samples: number;
}

export interface Calibration {
  factor: number;
  samples: number;
  per_kind: KindFactor[];
  window_days: number;
}

export const planCalibration = (days?: number) =>
  invoke<Calibration>("plan_calibration", { days: days ?? null });

// ---- browser-extension clip server (A7) ----------------------------------
// A loopback HTTP listener the manifest-V3 extension in `browser-extension/`
// talks to while Cortex is running. Settings → Experimental shows the address.

export interface ClipServerStatus {
  /** True once the loopback listener bound successfully. */
  running: boolean;
  /** "127.0.0.1:<port>", or null when the server never started. */
  addr: string | null;
}

/** Where (if anywhere) the browser-clip server is listening. */
export const clipServerStatus = () =>
  invoke<ClipServerStatus>("clip_server_status");

// ---- D8: extraction templates (Elicit-style table) --------------------------

export interface ExtractField {
  key: string;
  prompt: string;
}
export interface ExtractTemplate {
  id: string;
  name: string;
  description: string | null;
  fields: ExtractField[];
  created_at: number;
  updated_at: number;
}
export interface ExtractRun {
  id: string;
  template_id: string;
  template_name: string;
  title: string;
  fields: ExtractField[];
  item_ids: string[];
  model: string | null;
  status: "running" | "done" | "error";
  error: string | null;
  created_at: number;
  updated_at: number;
}
export interface ExtractCell {
  item_id: string;
  field_key: string;
  value: string;
  /** Verbatim supporting sentence from the paper card (model output). */
  evidence: string | null;
  /** Back-link to excerpts — set when the evidence matched or became a card. */
  excerpt_id: string | null;
  source: "model" | "manual";
  updated_at: number;
}
export interface ExtractItem {
  id: string;
  title: string;
  authors: string | null;
  year: number | null;
}
export interface ExtractResult {
  run: ExtractRun;
  items: ExtractItem[];
  cells: ExtractCell[];
}

export const extractTemplateList = () =>
  invoke<ExtractTemplate[]>("extract_template_list");
export const extractTemplateSave = (
  id: string | null,
  name: string,
  description: string | null,
  fields: ExtractField[],
) =>
  invoke<string>("extract_template_save", {
    id,
    name,
    description,
    fields,
  });
export const extractTemplateDelete = (id: string) =>
  invoke<void>("extract_template_delete", { id });
export const extractRunCreate = (
  templateId: string,
  itemIds: string[],
  title?: string,
) =>
  invoke<string>("extract_run_create", {
    templateId,
    itemIds,
    title: title ?? null,
  });
export const extractRunList = () => invoke<ExtractRun[]>("extract_run_list");
export const extractRunDelete = (id: string) =>
  invoke<void>("extract_run_delete", { id });
export const extractRunExecute = (runId: string) =>
  invoke<void>("extract_run_execute", { runId });
export const extractRunRetry = (runId: string, itemId: string) =>
  invoke<void>("extract_run_retry", { runId, itemId });
export const extractResultGet = (runId: string) =>
  invoke<ExtractResult>("extract_result_get", { runId });
export const extractResultCsv = (runId: string) =>
  invoke<string>("extract_result_csv", { runId });
/** Write the run's table as a CSV file (RFC-4180 + BOM) at `dest`. */
export const extractResultCsvFile = (runId: string, dest: string) =>
  invoke<void>("extract_result_csv_file", { runId, dest });
/** Write the run's table as a styled .xlsx workbook (values + evidence cols). */
export const extractResultXlsxFile = (runId: string, dest: string) =>
  invoke<void>("extract_result_xlsx_file", { runId, dest });
export const extractCellSet = (
  runId: string,
  itemId: string,
  fieldKey: string,
  value: string,
) => invoke<void>("extract_cell_set", { runId, itemId, fieldKey, value });

// ---- 3.2: PRISMA 8-dimension extraction (review-grade, evidence-traced) -----

export interface PrismaCell {
  item_id: string;
  field_key: string;
  value: string;
  /** Verbatim supporting sentence from the paper card (model output). */
  evidence: string | null;
  /** Back-link to excerpts — set when the evidence matched or became a card. */
  excerpt_id: string | null;
  source: "model" | "manual";
  updated_at: number;
}
export interface PrismaRun {
  id: string;
  title: string;
  /** The FIXED 8-field list, snapshotted at run time. */
  fields: ExtractField[];
  model: string | null;
  status: "running" | "done" | "error";
  error: string | null;
  item_ids: string[];
  created_at: number;
  updated_at: number;
}
export interface PrismaResult {
  run: PrismaRun;
  items: ExtractItem[];
  cells: PrismaCell[];
}

export const prismaRunCreate = (itemIds: string[], title?: string) =>
  invoke<string>("prisma_run_create", { itemIds, title: title ?? null });
export const prismaRunList = () => invoke<PrismaRun[]>("prisma_run_list");
export const prismaRunDelete = (id: string) =>
  invoke<void>("prisma_run_delete", { id });
export const prismaRunExecute = (runId: string) =>
  invoke<void>("prisma_run_execute", { runId });
export const prismaRunRetry = (runId: string, itemId: string) =>
  invoke<void>("prisma_run_retry", { runId, itemId });
export const prismaResultGet = (runId: string) =>
  invoke<PrismaResult>("prisma_result_get", { runId });
export const prismaCellSet = (
  runId: string,
  itemId: string,
  fieldKey: string,
  value: string,
) => invoke<void>("prisma_cell_set", { runId, itemId, fieldKey, value });
/** Summary table as CSV (RFC-4180 + BOM) at `dest`. */
export const prismaResultCsvFile = (runId: string, dest: string) =>
  invoke<void>("prisma_result_csv_file", { runId, dest });
/** Summary table as Markdown at `dest`. */
export const prismaResultMdFile = (runId: string, dest: string) =>
  invoke<void>("prisma_result_md_file", { runId, dest });
/** Summary table as styled .xlsx workbook (values + evidence cols). */
export const prismaResultXlsxFile = (runId: string, dest: string) =>
  invoke<void>("prisma_result_xlsx_file", { runId, dest });

// ---- D6: summary tree --------------------------------------------------------

export interface SummaryTreeStatus {
  subject_ok: boolean;
  subject_missing: boolean;
  topic_fresh: number;
  topic_stale: number;
  topic_missing: number;
  source_fresh: number;
  source_stale: number;
  source_missing: number;
  /** Source-text chars covered by fresh source digests. */
  total_chars: number;
  model_configured: boolean;
}

export const summaryTreeStatus = (subjectId: string) =>
  invoke<SummaryTreeStatus>("summary_tree_status", { subjectId });
export const summaryTreeBuild = (subjectId: string, force?: boolean) =>
  invoke<SummaryTreeStatus>("summary_tree_build", {
    subjectId,
    force: force ?? false,
  });

// ---- D5: connector plugins (fetch + ingest primitives) ------------------------

export interface ConnectorConfigField {
  key: string;
  label: string;
  placeholder: string;
}

export interface ConnectorInfo {
  id: string;
  label: string;
  plugin: string;
  docs_url: string;
  /** none | bearer | header | query */
  auth_kind: string;
  key_note: string;
  config_fields: ConnectorConfigField[];
  configured: boolean;
  /** Saved non-secret config values, for re-editing. */
  config_values: Record<string, string>;
}

export interface ConnectorSyncSummary {
  fetched: number;
  imported: number;
  skipped: number;
  errors: string[];
}

export const connectorList = () =>
  invoke<ConnectorInfo[]>("connector_list");
export const connectorConfigure = (
  connectorId: string,
  key: string,
  config: Record<string, string>,
) => invoke<void>("connector_configure", { connectorId, key, config });
export const connectorDisconnect = (connectorId: string) =>
  invoke<void>("connector_disconnect", { connectorId });
export const connectorSync = (
  connectorId: string,
  subjectId: string,
  topicId?: string | null,
) =>
  invoke<ConnectorSyncSummary>("connector_sync", {
    connectorId,
    subjectId,
    topicId: topicId ?? null,
  });

// ---- 2.3: built-in research connectors (arXiv / PubMed) -----------------------

export interface ResearchConnector {
  id: string;
  name: string;
  desc: string;
  hint: string;
  supports_pdf: boolean;
}

export interface ResearchFetchSummary {
  fetched: number;
  imported: number;
  skipped: number;
  errors: string[];
  titles_sample: string[];
}

export const researchConnectorList = () =>
  invoke<ResearchConnector[]>("research_connector_list");
export const researchConnectorFetch = (
  connectorId: string,
  query: string,
  max: number,
  subjectId: string,
  topicId?: string | null,
  withPdf = false,
) =>
  invoke<ResearchFetchSummary>("research_connector_fetch", {
    connectorId,
    query,
    max,
    subjectId,
    topicId: topicId ?? null,
    withPdf,
  });

// ---- L4: research chat — a conversation with the literature -----------------
// Distinct from `literatureAiSearch` (one question → one ranked list). A thread
// remembers what was asked, and every answer carries the tool calls behind it.

export interface ResearchThread {
  id: string;
  title: string;
  created_at: number;
  updated_at: number;
  turns: number;
}

export interface ResearchMessage {
  id: string;
  thread_id: string;
  /** user | agent | tool */
  role: string;
  text: string;
  tool: string | null;
  /** For tool rows: the arguments the model passed (JSON), so you can judge
   *  what was asked, not only what came back. */
  args: string | null;
  created_at: number;
}

export interface ResearchAskOutcome {
  thread_id: string;
  title: string;
  reply: string;
  steps: number;
  took_ms: number;
}

export const researchThreads = () => invoke<ResearchThread[]>("research_threads");

export const researchThreadMessages = (threadId: string, limit = 300) =>
  invoke<ResearchMessage[]>("research_thread_messages", { threadId, limit });

export const researchThreadDelete = (threadId: string) =>
  invoke<void>("research_thread_delete", { threadId });

export const researchAsk = (input: string, threadId?: string | null) =>
  invoke<ResearchAskOutcome>("research_ask", { threadId: threadId ?? null, input });

// ---- prompt lab (edit a prompt, compare two versions, score both) ----
export interface PromptInfo {
  key: string;
  label: string;
  group: string;
  /** Effective text — what the next model call will actually use. */
  body: string;
  default_body: string;
  /** A realistic input the lab can run without needing your data. */
  sample: string;
  custom: boolean;
}

export interface PromptRun {
  /** Cleaned text — what a real call would consume. */
  text: string;
  /** Untouched provider content (reasoning blocks intact). */
  raw: string;
  /** True when cleaning changed something (a `<think>` block was removed). */
  cleaned: boolean;
  finish_reason: string | null;
  usage: unknown | null;
  ms: number;
  chars: number;
  model: string;
}

export interface PromptEval {
  id: string;
  prompt_key: string;
  name: string;
  input: string;
  rules: string;
  created_at: number;
}

export interface PromptCaseResult {
  output: string;
  passed: boolean;
  failures: string[];
  ms: number;
}

export interface PromptEvalRow {
  eval_id: string;
  eval_name: string;
  a: PromptCaseResult;
  b: PromptCaseResult;
}

export interface PromptVariantScore {
  label: string;
  passed: number;
  total: number;
}

export interface PromptEvalOutcome {
  run_id: string;
  a: PromptVariantScore;
  b: PromptVariantScore;
  rows: PromptEvalRow[];
}

export const promptList = () => invoke<PromptInfo[]>("prompt_list");
export const promptSave = (key: string, body: string) =>
  invoke<void>("prompt_save", { key, body });
export const promptReset = (key: string) => invoke<void>("prompt_reset", { key });
/** `body` is a draft — pass null/undefined to run the saved prompt. */
export const promptRun = (key: string, input: string, body?: string | null) =>
  invoke<PromptRun>("prompt_run", { key, input, body: body ?? null });

export const promptEvals = (key: string) => invoke<PromptEval[]>("prompt_evals", { key });
export const promptEvalAdd = (key: string, name: string, input: string, rules: string) =>
  invoke<PromptEval>("prompt_eval_add", { key, name, input, rules });
export const promptEvalDelete = (id: string) => invoke<void>("prompt_eval_delete", { id });
export const promptEvalRun = (key: string, bodyA?: string | null, bodyB?: string | null) =>
  invoke<PromptEvalOutcome>("prompt_eval_run", { key, bodyA: bodyA ?? null, bodyB: bodyB ?? null });

export interface PromptEvalRunSummary {
  run_id: string;
  label_a: string;
  label_b: string;
  /** [passed, total] */
  a: [number, number];
  b: [number, number];
  created_at: number;
}

/** Past comparisons for one prompt — "it scored better last week" needs data. */
export const promptEvalHistory = (key: string, limit = 8) =>
  invoke<PromptEvalRunSummary[]>("prompt_eval_history", { key, limit });

// ---- transparency: what retrieval actually did ----
export interface TraceHit {
  chunk_id: string;
  source_id: string;
  source_name: string;
  loc: string | null;
  score: number;
  preview: string;
}

export interface RetrievalTrace {
  query: string;
  /** How the question was tokenized (CJK becomes character bigrams). */
  terms: string[];
  embed_provider: string;
  embed_ok: boolean;
  embed_error: string | null;
  vector: TraceHit[];
  keyword: TraceHit[];
  fused: TraceHit[];
  final_hits: TraceHit[];
  rerank: boolean;
  subject_name: string | null;
  ms: number;
}

/** Read-only replay of the retrieval path: terms → vector → keyword → fusion →
 *  final cut. `subjectId` null/empty searches the whole library. */
export const retrievalTrace = (query: string, subjectId?: string | null, k?: number) =>
  invoke<RetrievalTrace>("retrieval_trace", {
    query,
    subjectId: subjectId ?? null,
    k: k ?? null,
  });

// ---- growth profile — a human-owned object over learning signals ----------
// Derived from real BKT mastery / calibration / tutor-misjudgements. The model
// only ever *proposes*; a row exists only after the user confirms.

export interface GrowthEvidence {
  source: string; // knowledge_node | tutor_misjudged | calibration | manual
  ref: string; // backing row id (or subject id for calibration)
  excerpt: string; // traceable snippet
}

export interface GrowthItem {
  id: string;
  subject_id: string;
  subject_name: string;
  kind: string; // goal | misconception | review | strength
  title: string;
  detail: string;
  status: string; // active | paused | resolved | dismissed
  origin: string; // derived | manual
  evidence: GrowthEvidence[];
  created_at: number;
  updated_at: number;
  resolved_at: number | null;
}

export interface GrowthProposal {
  subject_id: string;
  subject_name: string;
  kind: string;
  title: string;
  detail: string;
  evidence: GrowthEvidence[];
}

/** Propose items from real data (no insert). subjectId null = every subject. */
export const growthDerive = (subjectId?: string | null) =>
  invoke<GrowthProposal[]>("growth_derive", { subjectId: subjectId ?? null });

/** List items. subjectId null = cross-subject; status null = every lifecycle. */
export const growthList = (subjectId?: string | null, status?: string | null) =>
  invoke<GrowthItem[]>("growth_list", {
    subjectId: subjectId ?? null,
    status: status ?? null,
  });

export const growthAdd = (
  subjectId: string,
  kind: string,
  title: string,
  detail: string,
  evidence?: GrowthEvidence[],
  origin?: string,
) =>
  invoke<GrowthItem>("growth_add", {
    subjectId,
    kind,
    title,
    detail,
    evidenceJson: evidence ? JSON.stringify(evidence) : null,
    origin: origin ?? null,
  });

export const growthSetStatus = (id: string, status: string) =>
  invoke<void>("growth_set_status", { id, status });

export const growthRemove = (id: string) =>
  invoke<void>("growth_remove", { id });

/** Markdown export of live items (active + paused). subjectId null = all. */
export const growthExport = (subjectId?: string | null) =>
  invoke<string>("growth_export", { subjectId: subjectId ?? null });

// ---- visuals (mind map / charts generated from a source) --------------------

export interface VisualResult {
  id: string;
  title: string;
  kind: string; // mindmap | chart
  svg: string;
}

/** Mind map from a source's extracted text; stored as a material (kind=mindmap). */
export const mindmapGenerate = (sourceId: string) =>
  invoke<VisualResult>("mindmap_generate", { sourceId });

/** chartType: flow | comparison | matrix; stored as a material (kind=chart). */
export const chartGenerate = (sourceId: string, chartType: string) =>
  invoke<VisualResult>("chart_generate", { sourceId, chartType });

// ---- external knowledge bases (KBP-style 外挂大脑) ---------------------------

export interface KbConn {
  id: string;
  name: string;
  kind: string; // dify | fastgpt | mcp
  base_url: string;
  dataset_id: string;
  args_json: string;
  description: string;
  enabled: boolean;
  created_at: number;
  has_key: boolean;
}

export interface KbHit {
  kb: string;
  source: string;
  content: string;
  score: number;
}

/** One KB's answer inside a merged search — failures stay isolated per KB. */
export interface KbSearchReport {
  name: string;
  kind: string;
  hits: KbHit[];
  error: string | null;
}

export const kbList = () => invoke<KbConn[]>("kb_list");

export const kbAdd = (
  name: string,
  kind: string,
  baseUrl: string,
  datasetId: string,
  apiKey: string | null,
  description: string,
  argsJson?: string | null,
) =>
  invoke<KbConn>("kb_add", {
    name,
    kind,
    baseUrl,
    datasetId,
    apiKey: apiKey || null,
    description,
    argsJson: argsJson ?? null,
  });

/** apiKey semantics: null = keep existing, "" = clear, text = replace. */
export const kbUpdate = (
  id: string,
  name: string,
  baseUrl: string,
  datasetId: string,
  description: string,
  argsJson: string | null,
  apiKey: string | null,
) =>
  invoke<void>("kb_update", {
    id,
    name,
    baseUrl,
    datasetId,
    description,
    argsJson,
    apiKey,
  });

export const kbSetEnabled = (id: string, enabled: boolean) =>
  invoke<void>("kb_set_enabled", { id, enabled });

export const kbRemove = (id: string) => invoke<void>("kb_remove", { id });

export const kbQuery = (id: string, query: string, topK?: number) =>
  invoke<KbHit[]>("kb_query", { id, query, topK: topK ?? null });

export const kbSearchAll = (query: string, topK?: number) =>
  invoke<KbSearchReport[]>("kb_search_all", { query, topK: topK ?? null });

/** Description protocol as prompt text (enabled KBs, one line each). */
export const kbContext = () => invoke<string>("kb_context");

// ---- cloze (遮挡) flashcards -------------------------------------------------

export interface ClozeResult {
  material_id: string;
  count: number;
}

/** Generate occlusion cards from a source; saved as a flashcards material. */
export const clozeGenerate = (sourceId: string, count?: number) =>
  invoke<ClozeResult>("cloze_generate", { sourceId, count: count ?? null });

// ---- preset agent roles (预置角色库) ------------------------------------------

export interface PresetRole {
  id: string;
  name: string;
  description: string;
  system_prompt: string;
  skills: string[];
  tools: string[];
  tool_tier: string;
}

export const rolesList = () => invoke<PresetRole[]>("roles_list");

/** Instantiate a preset as a normal editable agent (skills auto-enabled). */
export const rolesInstantiate = (presetId: string, name?: string) =>
  invoke<Agent>("roles_instantiate", { presetId, name: name ?? null });
