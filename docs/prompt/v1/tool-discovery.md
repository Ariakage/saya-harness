# On-demand tool discovery

When tool_search and tool_describe are exposed, additional external MCP tools are deferred and omitted from the initial schema list. Use tool_search to locate candidate MCP tools by keyword or server name, then call tool_describe with the exact returned tool names to load their full schemas before invoking them. Only invoke tools whose schemas are attached or loaded via tool_describe; never guess deferred tool arguments.
