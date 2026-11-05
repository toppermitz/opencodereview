import { FolderGit2, GitPullRequest, KeyRound, Plug } from "lucide-react";
import type { Settings } from "../../../types";

export const SETTINGS_SECTIONS = [
  { id: "accounts", title: "Provedores", icon: KeyRound },
  { id: "repositories", title: "Acesso a repositórios", icon: FolderGit2 },
  { id: "selection", title: "Seleção de MRs", icon: GitPullRequest },
  { id: "integrations", title: "Integrações", icon: Plug },
] as const;

export type SettingsSectionId = (typeof SETTINGS_SECTIONS)[number]["id"];
export type NumericSetting =
  | "pollIntervalSeconds"
  | "reviewConcurrency"
  | "reviewDeepThreshold"
  | "reviewChunkSize"
  | "reviewParallelChunks"
  | "reviewMaxFiles"
  | "reviewArchiveRetentionDays";
export type SettingsForm = Omit<Settings, NumericSetting> & Record<NumericSetting, number | string>;
export const settingField = (key: string) =>
  key.replace(/_([a-z])/g, (_, letter: string) => letter.toUpperCase()) as keyof Settings;
export const serializeSetting = (value: unknown) => (typeof value === "object" ? JSON.stringify(value) : String(value));
export const settingSection = (_key: string): SettingsSectionId => "selection";
