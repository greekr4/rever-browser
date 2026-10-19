// One line appended to every user turn. The system prompt is sent only once.
export type ToolProfileId = 'browse' | 'reverse' | 'probe'

export function toolProfileTurnLine(profile: ToolProfileId): string {
  return `[tool-profile: ${profile}] Tools outside this profile are not loaded. Call tool_profile_set to switch between browse and reverse. Probe is selected by the user.`
}
