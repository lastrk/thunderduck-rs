# Code Search Tools

> Two MCP-backed search tools are preinstalled in the devcontainer. They answer
> different kinds of questions — pick the right one. Prefer either over raw grep
> for code questions.

## codegraph — structural queries over a parsed AST/symbol graph

Use when you have a symbol name or want to trace relationships. Exact,
deterministic, AST-backed — answers grep can't give (callers, callees, impact).

- "Where is `SqlGenerator` defined?" → `codegraph_search`
- "What calls `visit_join`?" → `codegraph_callers`
- "What does `convert_aggregate` call?" → `codegraph_callees`
- "What would break if I change `LogicalPlan`?" → `codegraph_impact`
- "Show me the signature / source of `to_sql`" → `codegraph_node`
- "Give me focused context for an area" → `codegraph_context`

## semble — semantic / hybrid search over code chunks

Use when you don't know the symbol name yet, just intent. Fuzzy, intent-based;
good for unfamiliar areas; can also index remote git URLs.

- "How does session lifecycle work?" → `semble.search`
- "Where do we stream Arrow back to the client?" → `semble.search`
- "Find code similar to this snippet" → `semble.find_related`
- **Pass the project root as `repo`** (the working directory, e.g. `/workspace`) — without it, semble errors with "No repo specified and no default index."

## Rule of thumb

Named symbol or relationship → codegraph; fuzzy intent or unfamiliar area →
semble. If semble surfaces a candidate symbol, hand it to codegraph for the
precise structural follow-up.
