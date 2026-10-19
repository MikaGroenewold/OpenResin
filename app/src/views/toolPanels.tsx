// Side panels contributed by optional geometry tools (filled in as tools land).
import type { ReactNode } from "react";
import { BooleanPanel, CutPanel, HollowPanel, LabelPanel, OrientPanel, PlatformPanel, RepairPanel } from "./GeometryPanels";

export const toolPanels: Partial<Record<string, ReactNode>> = {
  repair: <RepairPanel />,
  cut: <CutPanel />,
  boolean: <BooleanPanel />,
  hollow: <HollowPanel />,
  orient: <OrientPanel />,
  label: <LabelPanel />,
  platform: <PlatformPanel />,
};
