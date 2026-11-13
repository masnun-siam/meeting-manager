import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { FoldersSection } from './FoldersSection';

const info = (vault_dir = '/Users/me/Vault', vault_folder = 'Meetings') => ({
  kind: 'info' as const,
  info: { recordings_dir: '/Users/me/Recordings', vault_dir, vault_folder, store: { ok: true as const } },
});

describe('FoldersSection', () => {
  it('shows both paths as read-only text with the install hint', () => {
    render(<FoldersSection view={info()} />);

    expect(screen.getByText('/Users/me/Recordings').tagName).toBe('DD');
    expect(screen.getByText('/Users/me/Vault/Meetings').tagName).toBe('DD');
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.getByText('meeting install').tagName).toBe('CODE');
    expect(screen.getByText(/Change with/)).toHaveTextContent('Change with meeting install');
  });

  it('joins the vault folder without doubling a slash or adding one for an empty folder', () => {
    const { rerender } = render(<FoldersSection view={info('/Vault/', 'Notes')} />);
    expect(screen.getByText('/Vault/Notes')).toBeInTheDocument();

    rerender(<FoldersSection view={info('/Vault', '')} />);
    expect(screen.getByText('/Vault')).toBeInTheDocument();
  });

  it('shows a not_configured message in place of the paths', () => {
    render(<FoldersSection view={{ kind: 'failed', message: 'Run meeting install first' }} />);

    expect(screen.getByText('Run meeting install first')).toBeInTheDocument();
    expect(screen.queryByText('Recordings directory')).not.toBeInTheDocument();
  });
});
