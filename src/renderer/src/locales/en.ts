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
  'toolbar.theme.dark': 'Dark',
  'toolbar.theme.light': 'White',
  'toolbar.theme.system': 'Com',
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
  'lang.label': 'Language',

  // Common
  'common.cancel': 'Cancel',
  'common.edit': 'Edit',
  'common.delete': 'Delete',
  'common.remove': 'Remove',
  'common.stop': 'Stop',
  'common.clear': 'Clear',
  'common.moveUp': 'Move up',
  'common.moveDown': 'Move down',

  // Panels (empty states)
  'panel.noConsole': 'No console output',
  'panel.noExceptions': 'No exceptions',
  'panel.noWs': 'No WS connections',
  'panel.noFrames': 'No frames',
  'panel.selectConnection': 'Select a connection',
  'history.filter': 'Filter by URL or title',

  // Traffic
  'traffic.clear': 'Clear captured traffic',
  'traffic.filter': 'Filter URL or method…',
  'traffic.method': 'Method',
  'traffic.status': 'Status',
  'traffic.type': 'Type',
  'traffic.url': 'URL',
  'traffic.size': 'Size',
  'traffic.sendToRepeater': 'Send to Repeater',

  // Traffic detail drawer
  'drawer.request': 'Request',
  'drawer.response': 'Response',
  'drawer.queryParams': 'Query params',
  'drawer.responseBody': 'Response body',

  // JSON body viewer
  'json.pretty': 'Pretty',
  'json.raw': 'Raw',
  'json.copy': 'Copy',
  'json.copied': 'Copied',
  'json.items': '{n} items',
  'json.keys': '{n} keys',
  'json.more': '… {n} more',

  // Repeater
  'repeater.empty': 'In Traffic, click the "↻R" (Send to Repeater) button on a request row to load it here.',
  'repeater.removeHeader': 'Remove header',

  // Chat
  'chat.input': 'input',
  'chat.output': 'output',
  'chat.switchModel': 'Switch model for this session',
  'chat.newConversation': 'Start a new conversation (kills the current agent session)',
  'chat.placeholder': 'Type a message…',
  'chat.historyTitle': 'Conversation history',
  'chat.history': 'History',
  'chat.deleteConversation': 'Delete conversation',
  'chat.chooseAgent': 'Choose AI agent',
  'chat.chooseAgentShort': 'Choose agent',

  // Macro editor
  'macro.goalPlaceholder': 'Login then dump the profile API',
  'macro.steps': 'Steps',
  'macro.removeStep': 'Remove step',
  'macro.waitForPlaceholder': 'wait for selector (optional)',
  'macro.waitForTitle': 'Poll until this CSS selector matches, then run the step. Fails the step after 10s.',
  'macro.delayPlaceholder': 'delay ms',
  'macro.delayTitle': 'Extra pause before the step, applied after waitFor',
  'macro.saveAsPlaceholder': 'save as',
  'macro.saveAsTitle': "Store this step's result under this name; use it later as {{name}}",

  // Pipeline editor
  'pipeline.ifLast': 'if last',
  'pipeline.valuePlaceholder': 'value / regex',
  'pipeline.goalPlaceholder': 'Probe endpoint, branch on 200 vs 403',

  // Template editor
  'template.goalPlaceholder': "Analyze this site's auth flow",
  'template.bodyPlaceholder': 'Text inserted into the agent chat input when you click Use.',

  // First-run agent onboarding
  'onboard.title': 'Connect your AI provider',
  'onboard.desc': 'Rever drives the browser through an AI agent. Use a subscription you already have, or paste an API key.',
  'onboard.scanning': 'Checking your machine…',
  'onboard.ready': '{n} ready',
  'onboard.use': 'Use',
  'onboard.inUse': 'In use',
  'model.search': 'Search models…',
  'model.noMatch': 'No models match',
  'onboard.skip': 'Skip for now',
  'onboard.recheck': 'Re-check',
  'onboard.save': 'Save',
  'onboard.keyPlaceholder': 'Paste API key…',
  'onboard.installWith': 'Install with',
  'onboard.signInWith': 'Sign in with',
  'onboard.copy': 'Copy',
  'onboard.copied': 'Copied \u2713',
  'onboard.plan': '{plan} plan',
  'onboard.status.ready': 'Ready',
  'onboard.status.auto': 'Connected automatically',
  'onboard.status.not-installed': 'Not installed',
  'onboard.status.needs-key': 'Needs API key',
  'onboard.status.needs-login': 'Sign-in required',
  'onboard.status.auth-failed': 'Key rejected',
  'onboard.status.failed': 'Failed to start',

  // Address-bar toolbar (App)
  'toolbar.back': 'Back',
  'toolbar.forward': 'Forward',
  'toolbar.reload': 'Reload',
  'toolbar.hardReload': 'Hard Reload (clear cache)',
  'toolbar.go': 'Go',
  'toolbar.findInPage': 'Find in page',

  // Proxy button
  'proxy.label': 'Proxy',
  'proxy.labelActive': 'Proxy: {host}:{port}',
  'proxy.setTitle': 'Set a proxy for the browser',
  'proxy.editTitle': 'Proxy: {scheme}://{host}:{port} — click to edit',
  'proxy.tabProxy': 'Tab proxy',
  'proxy.scheme': 'Scheme',
  'proxy.host': 'Host',
  'proxy.port': 'Port',
  'proxy.user': 'User',
  'proxy.optional': '(optional)',
  'proxy.password': 'Password',
  'proxy.disable': 'Disable',
  'proxy.apply': 'Apply',
  'proxy.openTab': 'Open proxy tab',
  'proxy.errHost': 'Host is required',
  'proxy.errPort': 'Port must be 1–65535',
  'proxy.errApply': 'Failed to apply proxy',
  'proxy.noteEdit':
    'This tab runs in its own session (separate cookies, incognito-style). Changes apply to this tab only and reload the page.',
  'proxy.noteNew':
    'Opens the current page in a new tab with its own cookies and storage (like incognito), routed through this proxy. Other tabs are unaffected.',

  // Bot-detection probes
  'botcheck.trigger': 'Bot check',
  'botcheck.triggerTitle': 'Open bot detection test sites',
  'botcheck.menuTitle': 'Bot detection probes',
  'botcheck.sannysoft.desc': 'Baseline stealth (webdriver / plugins / UA)',
  'botcheck.creepjs.desc': 'Full-stack fingerprint (canvas / audio / font / WebGL)',
  'botcheck.browserleaks.desc': 'Individual leaks (Canvas / WebGL / WebRTC)',
  'botcheck.amiunique.desc': 'Uniqueness score',
  'botcheck.pixelscan.desc': 'Consistency cross-check (UA / OS / WebGL)',

  // Cookies panel section tabs
  'cookies.tab.cookies': 'Cookies',
  'cookies.tab.local': 'localStorage',
  'cookies.tab.session': 'sessionStorage',
  'cookies.refresh': 'Refresh',
}

export type TKey = keyof typeof en
