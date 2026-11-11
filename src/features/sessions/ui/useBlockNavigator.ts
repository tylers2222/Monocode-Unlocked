import { useCallback, useRef, useState } from "react";

type NavigateBlock = (blockId: string | null, query?: string) => boolean;

/** Holds the navigator AgentTranscript hands over through `onNavigateReady`. */
export function useBlockNavigator() {
  const navigateBlockRef = useRef<NavigateBlock | null>(null);
  const [navigatorReady, setNavigatorReady] = useState(false);
  const onNavigateReady = useCallback((navigate: NavigateBlock) => {
    navigateBlockRef.current = navigate;
    setNavigatorReady(true);
  }, []);
  return { navigateBlockRef, navigatorReady, onNavigateReady };
}
