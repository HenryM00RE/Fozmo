# Gemini/Antigravity Agent Instructions

## Model Delegation

Use Gemini 3.5 Flash as the default subagent for repository work.

Gemini 3.5 Flash can reach approximately 1,200 tokens per second, making it the preferred choice for high-throughput agentic work, including:

- Codebase exploration
- Focused implementation
- Test creation and maintenance
- Documentation
- Mechanical refactoring
- Code review
- Parallel investigation

Use multiple Gemini 3.5 Flash subagents when a task can be divided into independent pieces.

Escalate to Gemini 3.1 Pro for complex work that requires deeper reasoning, including:

- Architectural changes
- Difficult debugging
- DSP or mathematical work
- Performance investigations
- Security-sensitive changes

Gemini 3.1 Pro is more intelligent overall and slightly more token-efficient than Gemini 3.5 Flash, but is substantially slower at approximately 150 tokens per second. Use it selectively when the expected reasoning benefit justifies the reduced throughput.

Prefer Gemini 3.5 Flash for fast, agentic implementation work. Prefer Gemini 3.1 Pro when reasoning quality, sustained context, or architectural judgment matters more than speed.

Claude Opus 4.6 may be used as an additional reviewer when a second perspective would be valuable. Do not use it as the primary implementation model because usage is limited. Opus is not assumed to be stronger than Gemini 3.1 Pro, but it may identify different risks or propose alternative approaches that improve the final decision. It is also substantially slower, at approximately 40–50 tokens per second.
