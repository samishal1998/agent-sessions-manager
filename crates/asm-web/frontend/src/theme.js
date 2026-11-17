// The app's accent in two modes, over Hearth's own palette: its page and cards
// are neutral, and raised surfaces and panels take a slight tint of this accent.
// Sizes come from Hearth's `high` density (set on HTheme). Every accent text
// pair is at least 4.5:1 on every surface.

const DARK = {
  '--h-accent': '#a689f5',
  '--h-accent-hover': '#d7c9fa',
  '--h-accent-text': '#cebcf8',
  '--h-on-accent': '#211735',
}

const LIGHT = {
  '--h-accent': '#6d3fc8',
  '--h-accent-hover': '#5b2fb0',
  '--h-accent-text': '#5b2fb0',
  '--h-on-accent': '#ffffff',
}

export const TOKENS = { dark: DARK, light: LIGHT }
