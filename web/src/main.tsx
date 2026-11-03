import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
// Fonts are bundled into the build: the device works without internet access.
import '@fontsource-variable/geist';
import './styles/app.css';
import { App } from './App';

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
