import { useState } from 'react';
import Markdown from 'react-markdown';
import rehypeHighlight from 'rehype-highlight';
import remarkGfm from 'remark-gfm';

export default function MarkdownContent({ children }: { children: string }) {
  return (
    <div className="text-sm text-foreground leading-relaxed
      [&_h1]:text-xl [&_h1]:font-bold [&_h1]:my-3
      [&_h2]:text-lg [&_h2]:font-bold [&_h2]:my-2.5
      [&_h3]:text-base [&_h3]:font-bold [&_h3]:my-2
      [&_h4]:text-sm [&_h4]:font-bold [&_h4]:my-1.5
      [&_p]:my-1.5
      [&_ul]:list-disc [&_ul]:pl-5 [&_ul]:my-1.5
      [&_ol]:list-decimal [&_ol]:pl-5 [&_ol]:my-1.5
      [&_li]:my-0.5
      [&_blockquote]:border-l-[3px] [&_blockquote]:border-primary [&_blockquote]:pl-3 [&_blockquote]:my-2 [&_blockquote]:text-muted-foreground [&_blockquote]:italic
      [&_hr]:!border-border [&_hr]:my-4
      [&_a]:text-primary [&_a]:underline
      [&_strong]:font-bold [&_em]:italic [&_del]:line-through
      [&_img]:max-w-full [&_img]:rounded
      [&_table]:w-full [&_table]:border-collapse [&_table]:my-2 [&_table]:text-xs
      [&_th]:border [&_th]:border-border [&_th]:px-2 [&_th]:py-1.5 [&_th]:bg-secondary [&_th]:font-medium [&_th]:text-left
      [&_td]:border [&_td]:border-border [&_td]:px-2 [&_td]:py-1.5
      [&_tr]:border-border
      [&_code]:bg-secondary [&_code]:px-1.5 [&_code]:py-0.5 [&_code]:rounded [&_code]:text-xs [&_code]:font-mono [&_code]:text-foreground/90
      [&_pre]:my-3 [&_pre_code]:!bg-transparent [&_pre_code]:!p-0 [&_pre_code]:!text-xs
    ">
      <Markdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{ pre: PreBlock }}
      >
        {children}
      </Markdown>
    </div>
  );
}

// ── Code block with copy button ──────────────────────────────────────

function PreBlock({ children }: { children?: React.ReactNode }) {
  const [copied, setCopied] = useState(false);

  return (
    <div className="bg-secondary rounded-lg overflow-hidden my-3">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-border/50">
        <span className="text-[10px] text-muted-foreground font-mono">code</span>
        <button
          onClick={async () => {
            await navigator.clipboard.writeText(extractText(children));
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
          }}
          className="text-[10px] text-muted-foreground hover:text-foreground transition-colors"
        >
          {copied ? '已复制 ✓' : '复制'}
        </button>
      </div>
      <pre className="!bg-transparent !p-3 !m-0 text-xs overflow-x-auto font-mono leading-relaxed">
        {children}
      </pre>
    </div>
  );
}

function extractText(node: unknown): string {
  if (typeof node === 'string') return node;
  if (Array.isArray(node)) return (node as any[]).map(extractText).join('');
  if (node && typeof node === 'object' && 'props' in (node as any)) {
    return extractText((node as any).props.children);
  }
  return '';
}
