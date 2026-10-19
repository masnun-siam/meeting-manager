import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { MicPermission } from './MicPermission';

const root = document.getElementById('root');
if (!root) throw new Error('Permission page root element is missing');

createRoot(root).render(
  <StrictMode>
    <MicPermission />
  </StrictMode>,
);
