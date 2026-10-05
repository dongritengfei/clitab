import React from 'react';
import ReactDOM from 'react-dom/client';
import '@fontsource/jetbrains-mono/400.css';
import '@fontsource/jetbrains-mono/400-italic.css';
import '@fontsource/jetbrains-mono/700.css';
import '@fontsource/jetbrains-mono/700-italic.css';
import App from './App';

// xterm measures its glyph cells once, when the terminal opens, so the
// bundled fonts must be loaded before any Terminal mounts — otherwise the
// fallback's metrics stay baked in until the next resize.
Promise.all([
  document.fonts.load('14px "JetBrains Mono"'),
  document.fonts.load('bold 14px "JetBrains Mono"'),
  document.fonts.load('14px "JetBrainsMono Nerd Font Mono"'),
])
  .then(() => document.fonts.ready)
  .then(() => {
    ReactDOM.createRoot(document.getElementById('root')!).render(
      <React.StrictMode>
        <App />
      </React.StrictMode>
    );
  });
