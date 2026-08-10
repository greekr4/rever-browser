// English UI strings — the source of truth. `ko.ts` mirrors these keys.
// Placeholders use {name} and are filled by the t() helper.
export const en = {
  // Agent pane
  'agent.chat': 'Chat (ACP)',
  'agent.terminal': 'Terminal (CLI)',

  // Terminal (CLI) onboarding
  'cli.title': 'Rever in your terminal',
  'cli.desc': "Run Claude Code in a real terminal, already wired to Rever's browser and traffic tools.",
  'cli.start': 'Start Claude Code →',
  'cli.skillPrompt': 'Want /rever in any Claude Code session? Run this once:',
  'cli.copy': 'Copy',
  'cli.copied': 'Copied ✓',
  'cli.installed': '✓ /rever skill already installed',
  'cli.requires': 'Requires the claude CLI (your own subscription). macOS/Linux.',
  'cli.guide': 'Guide',
  'cli.guideTitle': 'Show CLI mode guide',
  'cli.exited': 'Agent exited (code {code}).',
  'cli.restart': 'Restart',

  // Tabs & profiles
  'tab.new': 'New tab',
  'tab.close': 'Close tab',
  'tab.newInProfile': 'New tab in profile',
  'profile.openIn': 'Open in profile',
  'profile.newName': 'New profile name',
  'profile.persistent': '+ Persistent',
  'profile.incognito': '+ Incognito',
  'profile.persistentHint': 'Create a persistent profile (survives restart)',
  'profile.incognitoHint': 'Create an incognito profile (in-memory, cleared on quit)',
  'profile.importFrom': 'Import profile from browser',
  'profile.delete': 'Delete profile',
  'profile.incognitoTag': 'incognito',

  // Toolbar
  'toolbar.viewport.desktop': 'Desktop',
  'toolbar.viewport.mobile': 'Mobile',
  'toolbar.embedded': 'Embedded',
  'toolbar.external': 'External',
  'toolbar.viewSource': 'View page source in a new tab (view-source:)',
  'toolbar.pick': 'Pick an element (copies selector + ref)',
  'toolbar.grab': 'Grab an element (screenshot + context → chat & markup)',
  'toolbar.addressPlaceholder': 'https://...',
  'toolbar.theme': 'Theme: {mode} — click to cycle System → Light → Dark',
  'toolbar.openChat': 'Open chat',
  'toolbar.collapseChat': 'Collapse chat',

  // Markup editor
  'markup.rect': 'Rectangle',
  'markup.arrow': 'Arrow',
  'markup.pen': 'Pen',
  'markup.undo': 'Undo',
  'markup.clear': 'Clear',
  'markup.copy': 'Copy',
  'markup.copied': 'Copied ✓',
  'markup.save': 'Save PNG',
  'markup.close': 'Close',

  // Cookies panel — browser import
  'cookies.importFrom': 'Import cookies from',
  'cookies.importBrowserTitle': 'Browser to import from',
  'cookies.importProfileTitle': 'Profile to import from',
  'cookies.domainsPlaceholder': 'domains, comma-separated (blank = all)',
  'cookies.domainsTitle': 'Only import cookies whose host contains one of these substrings',
  'cookies.import': 'Import',
  'cookies.importing': 'Importing…',

  // Language switcher
  'lang.label': 'Language'
}

export type TKey = keyof typeof en
