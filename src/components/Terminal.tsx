import React, { useEffect, useRef } from 'react';
import { Terminal as XTerm } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';

interface TerminalProps {
  tabId: string;
  isActive: boolean;
  onOutput: (tabId: string, handler: (data: Uint8Array) => void) => void;
  onUnmount: (tabId: string) => void;
  onInput: (tabId: string, data: Uint8Array) => void;
  onResize: (tabId: string, rows: number, cols: number) => void;
}

export const Terminal: React.FC<TerminalProps> = ({
  tabId,
  isActive,
  onOutput,
  onUnmount,
  onInput,
  onResize,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<XTerm | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new XTerm({
      cursorBlink: true,
      fontSize: 14,
      fontFamily: '"JetBrains Mono", "Fira Code", "Cascadia Code", Menlo, monospace',
      theme: {
        background: '#1e1e2e',
        foreground: '#cdd6f4',
        cursor: '#f5e0dc',
        selectionBackground: '#585b7066',
      },
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.open(containerRef.current);

    // Try to load WebGL addon
    try {
      const webglAddon = new WebglAddon();
      webglAddon.onContextLoss(() => {
        webglAddon.dispose();
      });
      term.loadAddon(webglAddon);
    } catch (e) {
      console.warn('WebGL addon failed to load, using canvas renderer');
    }

    fitAddon.fit();
    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Handle input
    term.onData((data: string) => {
      const encoder = new TextEncoder();
      onInput(tabId, encoder.encode(data));
    });

    // Handle resize
    term.onResize(({ cols, rows }) => {
      onResize(tabId, rows, cols);
    });

    // Register output handler
    onOutput(tabId, (data: Uint8Array) => {
      term.write(data);
    });

    // Handle window resize
    const handleResize = () => {
      fitAddon.fit();
    };
    window.addEventListener('resize', handleResize);

    return () => {
      window.removeEventListener('resize', handleResize);
      onUnmount(tabId);
      term.dispose();
    };
  }, [tabId]);

  // Re-fit when becoming active
  useEffect(() => {
    if (isActive && fitAddonRef.current) {
      // Small delay to ensure container is visible
      setTimeout(() => {
        fitAddonRef.current?.fit();
      }, 50);
    }
  }, [isActive]);

  return <div ref={containerRef} className="terminal-container" />;
};
