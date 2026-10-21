-- 0071_writing_kit.sql — the writing toolbox and the upgrade ledger.
--
-- 知源's 应试写作有两件 Cortex 缺的东西：一个随手的工具箱（模板句型 /
-- 替换词库 / 过渡词 / 中式英语对照 / 评分标准），和一条「每次演示结果
-- 都留历史、精华自动回填」的回路——你采纳过的升格，本身就是最适合你
-- 的替换词库。Cortex 的 draft_review 只做「规则体检 + AI 升格 + 逐句
-- 改写」，改完即散：采纳了什么、忽略了什么，一个字节都没留下。
--
-- 两张表，职责分开：
--   writing_kit     工具箱。builtin 随启动 seed（按 category+title 幂等，
--                   绝不覆盖用户编辑过的同名条目）；harvest 是被采纳的
--                   升格自动回填的；user 是用户自己加的。
--   upgrade_history 升格账本。一次升格/改写一条：before → after、理由、
--                   采纳与否。harvest 的原料就是这里 accepted=1 的行。
CREATE TABLE IF NOT EXISTS writing_kit (
  id         TEXT PRIMARY KEY,
  category   TEXT NOT NULL,              -- 模板句型 | 替换词库 | 过渡词 | 中式英语对照 | 评分标准 | 升格实录
  title      TEXT NOT NULL,
  content    TEXT NOT NULL,
  source     TEXT NOT NULL,              -- builtin | user | harvest
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS writing_kit_cat_idx ON writing_kit(category, created_at);

CREATE TABLE IF NOT EXISTS upgrade_history (
  id         TEXT PRIMARY KEY,
  draft_id   TEXT NOT NULL REFERENCES drafts(id) ON DELETE CASCADE,
  kind       TEXT NOT NULL,              -- upgrade | rewrite
  before_text TEXT NOT NULL,
  after_text TEXT NOT NULL,
  reason     TEXT,
  accepted   INTEGER NOT NULL,           -- 0 | 1
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS upgrade_history_draft_idx ON upgrade_history(draft_id, created_at DESC);
