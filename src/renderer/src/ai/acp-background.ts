import type { SessionUpdate } from '@agentclientprotocol/sdk'

// Claude Code runs a turn on its own when a background task (a Bash with
// run_in_background, a background subagent) finishes after the user's turn
// ended. Newer claude-agent-acp streams that turn as ordinary session updates
// with no prompt in flight; these two predicates frame it for the chat.

// Output the user should see. Idle bookkeeping (command lists, usage, a late
// status refresh of an earlier turn's tool call) must not open a chat turn.
export function startsBackgroundTurn(update: SessionUpdate): boolean {
  switch (update.sessionUpdate) {
    case 'agent_message_chunk':
      return update.content.type !== 'text' || update.content.text.length > 0
    case 'agent_thought_chunk':
    case 'tool_call':
      return true
    default:
      return false
  }
}

// The agent reports a turn's result as a usage_update carrying its cost;
// mid-turn context-size updates carry none.
export function endsBackgroundTurn(update: SessionUpdate): boolean {
  return update.sessionUpdate === 'usage_update' && update.cost != null
}
