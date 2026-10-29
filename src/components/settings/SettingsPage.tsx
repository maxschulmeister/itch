import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { defaultThemeColors, parseCssColorToRgb } from "../../context/ThemeContext";
import {
  FolderIcon,
  SwatchIcon,
  KeyboardIcon,
  InfoIcon,
  IntegrationsIcon,
} from "../icons";
import { Button } from "../ui";
import { GeneralSettingsSection } from "./GeneralSettingsSection";
import { AppearanceSettingsSection } from "./EditorSettingsSection";
import { ShortcutsSettingsSection } from "./ShortcutsSettingsSection";
import { AboutSettingsSection } from "./AboutSettingsSection";
import { ToolsSettingsSection } from "./ToolsSettingsSection";
import { mod, isWindows } from "../../lib/platform";

type SettingsTab = "general" | "tools" | "editor" | "shortcuts" | "about";

const tabs: {
  id: SettingsTab;
  label: string;
  icon: typeof FolderIcon;
  shortcut: string;
}[] = [
  { id: "general", label: "Folder", icon: FolderIcon, shortcut: "1" },
  { id: "tools", label: "Integrations", icon: IntegrationsIcon, shortcut: "2" },
  { id: "editor", label: "Appearance", icon: SwatchIcon, shortcut: "3" },
  { id: "shortcuts", label: "Shortcuts", icon: KeyboardIcon, shortcut: "4" },
  { id: "about", label: "About", icon: InfoIcon, shortcut: "5" },
];

export function SettingsPage() {
  const [activeTab, setActiveTab] = useState<SettingsTab>("general");
  const scrollContainerRef = useRef<HTMLDivElement>(null);

  // The native window stays hidden until the saved theme has been applied.
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    const show = () => {
      if (cancelled) return;
      const root = document.documentElement;
      const color = getComputedStyle(root).getPropertyValue("--color-bg");
      const theme = root.classList.contains("dark") ? "dark" : "light";
      const background = parseCssColorToRgb(color) ??
        parseCssColorToRgb(defaultThemeColors[theme].bg)!;
      invoke("show_settings_window", { background }).catch((error) => {
        console.error("Failed to show Settings:", error);
      });
    };
    listen("focus-settings", show).then((fn) => {
      if (cancelled) {
        fn();
        return;
      }
      unlisten = fn;
      show();
    });
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  // Reset scroll position when tab changes
  useEffect(() => {
    if (scrollContainerRef.current) {
      scrollContainerRef.current.scrollTop = 0;
    }
  }, [activeTab]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey) {
        if (e.key === "1") {
          e.preventDefault();
          setActiveTab("general");
        } else if (e.key === "2") {
          e.preventDefault();
          setActiveTab("tools");
        } else if (e.key === "3") {
          e.preventDefault();
          setActiveTab("editor");
        } else if (e.key === "4") {
          e.preventDefault();
          setActiveTab("shortcuts");
        } else if (e.key === "5") {
          e.preventDefault();
          setActiveTab("about");
        }
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  return (
    <div className="h-full flex bg-bg w-full">
      {/* Sidebar - matches main Notes sidebar */}
      <div className="w-64 h-full bg-bg-secondary border-r border-border flex flex-col select-none">
        {/* Drag region */}
        {!isWindows && <div className="h-11 shrink-0" data-tauri-drag-region></div>}

        <div className={`px-4 pb-2 border-b border-border shrink-0${isWindows ? " pt-2" : ""}`}>
          <h1 className="h-7 flex items-center font-medium text-base">Settings</h1>
        </div>

        {/* Navigation tabs */}
        <nav className="flex-1 p-2 flex flex-col gap-1">
          {tabs.map((tab) => {
            const Icon = tab.icon;
            const isActive = activeTab === tab.id;
            return (
              <Button
                key={tab.id}
                onClick={() => setActiveTab(tab.id)}
                variant={isActive ? "secondary" : "ghost"}
                size="sm"
                className="justify-between gap-2.5 h-10 pr-3.5"
              >
                <div className="flex items-center gap-2.5">
                  <Icon className="w-4.5 h-4.5 stroke-[1.5]" />
                  {tab.label}
                </div>
                <div className="text-xs text-text-muted">
                  <span className="mr-0.5">{mod}</span>
                  {tab.shortcut}
                </div>
              </Button>
            );
          })}
        </nav>
      </div>

      {/* Main content area */}
      <div className="flex-1 flex flex-col bg-bg overflow-hidden">
        {/* Drag region */}
        {!isWindows && <div className="h-11 shrink-0" data-tauri-drag-region></div>}

        {/* Content - centered with max width */}
        <div
          ref={scrollContainerRef}
          className="flex-1 overflow-auto scrollbar-gutter-stable"
        >
          <div className={`w-full max-w-3xl mx-auto px-6 pb-6${isWindows ? " pt-2" : ""}`}>
            {activeTab === "general" && <GeneralSettingsSection />}
            {activeTab === "tools" && <ToolsSettingsSection />}
            {activeTab === "editor" && <AppearanceSettingsSection />}
            {activeTab === "shortcuts" && <ShortcutsSettingsSection />}
            {activeTab === "about" && <AboutSettingsSection />}
          </div>
        </div>
      </div>
    </div>
  );
}
