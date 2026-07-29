// Lightweight modal dialog — no external deps, pure Tailwind.
// Usage:
//   <Dialog open={show} onClose={() => setShow(false)} title="标题">
//     <p>内容</p>
//   </Dialog>

import { useEffect, useRef } from 'react';
import { X } from 'lucide-react';

interface DialogProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children: React.ReactNode;
  /** Prevents closing on overlay click / Escape when true */
  persistent?: boolean;
  className?: string;
}

export function Dialog({ open, onClose, title, children, persistent, className }: DialogProps) {
  const overlayRef = useRef<HTMLDivElement>(null);

  // Close on Escape
  useEffect(() => {
    if (!open || persistent) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [open, persistent, onClose]);

  // Lock body scroll when open
  useEffect(() => {
    if (open) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }
    return () => { document.body.style.overflow = ''; };
  }, [open]);

  if (!open) return null;

  return (
    <div
      ref={overlayRef}
      className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50 backdrop-blur-sm animate-fade-in"
      onClick={(e) => {
        if (!persistent && e.target === overlayRef.current) onClose();
      }}
    >
      <div
        className={`bg-background border border-border rounded-xl shadow-2xl w-full max-w-md mx-4 overflow-hidden animate-scale-in ${className ?? ''}`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        {title && (
          <div className="flex items-center justify-between px-5 py-3 border-b border-border">
            <span className="text-sm font-semibold text-foreground">{title}</span>
            {!persistent && (
              <button
                onClick={onClose}
                className="text-muted-foreground hover:text-foreground transition-colors p-0.5"
              >
                <X size={16} />
              </button>
            )}
          </div>
        )}

        {/* Body */}
        <div className="px-5 py-4">
          {children}
        </div>
      </div>
    </div>
  );
}
