// Hermes pattern: followBottom threshold scrolling with "back to bottom" button.
//
// When the user scrolls within 160px of the bottom → auto-follow new messages.
// When they scroll away → stop following, show a floating "↓" button.
// Uses requestAnimationFrame throttling (no scroll event spam).

import { useRef, useCallback, useState, useEffect } from 'react';

const FOLLOW_THRESHOLD = 160; // px from bottom — within this = "user is following"

export function useAutoScroll(deps: unknown[]) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [showBackToBottom, setShowBackToBottom] = useState(false);
  const followRef = useRef(true);
  const tickingRef = useRef(false);

  // Check if we're near the bottom
  const checkFollow = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    const distFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
    const following = distFromBottom < FOLLOW_THRESHOLD;
    followRef.current = following;
    setShowBackToBottom(!following && el.scrollHeight > el.clientHeight + 200);
  }, []);

  // Scroll event handler (throttled via rAF)
  const onScroll = useCallback(() => {
    if (!tickingRef.current) {
      tickingRef.current = true;
      requestAnimationFrame(() => {
        checkFollow();
        tickingRef.current = false;
      });
    }
  }, [checkFollow]);

  // Scroll to bottom (called on new messages, or manually)
  const scrollToBottom = useCallback((smooth = true) => {
    const el = containerRef.current;
    if (!el) return;
    el.scrollTo({ top: el.scrollHeight, behavior: smooth ? 'smooth' : 'instant' });
    followRef.current = true;
    setShowBackToBottom(false);
  }, []);

  // Auto-follow when deps change (new messages, streaming)
  useEffect(() => {
    if (followRef.current) {
      scrollToBottom(true);
    }
  }, deps); // eslint-disable-line react-hooks/exhaustive-deps

  return { containerRef, showBackToBottom, onScroll, scrollToBottom };
}
