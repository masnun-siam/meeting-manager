import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { Pill } from './Pill';
import '../theme.css';
import './page.css';

const root = document.getElementById('root');
if (!root) throw new Error('Indicator root element is missing');

createRoot(root).render(
  <StrictMode>
    <Pill />
  </StrictMode>,
);
