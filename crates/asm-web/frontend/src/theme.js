// The app's palette, as Hearth tokens: a deep night-sky purple with a rose
// accent (a Night Owl feel, tuned for contrast — every text pair here is at
// least 4.5:1 on every surface). `HTheme` applies these on the app's root;
// style.css reads them through its own short names.
export const NIGHT_OWL = {
  '--h-bg': '#130b1f',
  '--h-surface': '#1b1230',
  '--h-raised': '#241a3b',
  '--h-panel': 'rgba(27, 18, 48, 0.88)',
  '--h-border': 'rgba(214, 180, 255, 0.14)',
  '--h-border-strong': '#7a6a99',
  '--h-text': '#f3eefc',
  '--h-muted': '#c0b4d9',
  '--h-faint': '#a398bf',
  '--h-accent': '#d58ae8',
  '--h-accent-hover': '#e8a8f2',
  '--h-accent-text': '#eab4f5',
  '--h-on-accent': '#2a0f33',
  '--h-success': '#6ee7a8',
  '--h-warning': '#f2c96b',
  '--h-danger': '#ff8aa1',
  '--h-info': '#8fb4ff',
}

// The rose-to-purple wash behind the whole app, and the accent gradient for
// the few places that mark "you are here".
export const ACCENT_GRADIENT = 'linear-gradient(135deg, #c792ea, #f472b6)'
