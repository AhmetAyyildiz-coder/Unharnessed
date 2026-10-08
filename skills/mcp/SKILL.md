---
name: mcp
description: Use this to call tools on the user's MCP services.
---

Use:

```python
await mcp.list_connections()        # the user's services; their connectionId is the first argument below
await mcp.search_tools("search documents")
await mcp.describe_tool("notion", "notion-search")
await mcp.list_tools("notion")
await mcp.call_tool("notion", "notion-search", {"query": "roadmap"})
```

You cannot connect a service or use its keys: if one is missing, tell the user to connect it.

When not to use: `list_tools` to print every schema; search for the tool you need instead.
