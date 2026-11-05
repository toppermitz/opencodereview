### Fixed

- Reviews on Codex or Claude Sonnet/Haiku no longer fail runner preflight after the
  chunk model-display fix. Stage usage also keeps the engine chosen for that run when
  ChatGPT's engine setting changes mid-stage, including after an output-repair retry.
