# Project Rules

## Subagent model selection
When dispatching subagents, pick the model by task difficulty:
- **Moderate difficulty** → use **Opus**.
- **Smart grep / lightweight search** → use **Sonnet**.
- **Hard implementation** → use **Fable**.

## Output discipline
Applies to both responses and code comments:
- Do not narrate process history: wrong directions taken and reversed, mistakes made and fixed, or things that were previously broken. Subsequent implementers only need the current state, not how it got there.
- Do not add statements about disproven or contradictory data — describing what turned out to be wrong clutters the output and degrades overall performance.
- Overly verbose comments do more harm than good; comment only what the current code cannot say for itself.

## Comment policy
- Self-documenting code that is understandable on a first pass never gets a comment.
- Keep comments at the function declaration, and only if needed — i.e. the function signature is not obvious enough on its own.
- Comments are for clarification, not for writing down thoughts.
