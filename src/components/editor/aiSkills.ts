import type { ComponentType } from "react";
import {
  ArrowDownWideNarrowIcon,
  ArrowUpNarrowWideIcon,
  FeatherIcon,
  InfoIcon,
  LanguagesIcon,
  ListCollapseIcon,
  MicVocalIcon,
  PenLineIcon,
  SmilePlusIcon,
  SpellCheckIcon,
  WandSparklesIcon,
} from "../icons";
import type { AiRun } from "./AiAssist";

type Icon = ComponentType<{ className?: string }>;

/** An AI action of the menus: it runs an instruction or opens a sub-list. */
export interface AiSkill {
  label: string;
  icon?: Icon;
  run?: AiRun;
  /** A sub-list, for example the tones or the languages. */
  items?: AiSkill[];
}

export interface AiSkillGroup {
  label: string;
  skills: AiSkill[];
}

export const TONES = [
  "Academic",
  "Business",
  "Casual",
  "Childfriendly",
  "Confident",
  "Conversational",
  "Creative",
  "Emotional",
  "Excited",
  "Formal",
  "Friendly",
  "Funny",
  "Humorous",
  "Informative",
  "Inspirational",
  "Memeify",
  "Narrative",
  "Objective",
  "Persuasive",
  "Poetic",
];

const LANGUAGES = [
  "English",
  "Korean",
  "Chinese",
  "Japanese",
  "Spanish",
  "Russian",
  "French",
  "Portuguese",
  "German",
  "Italian",
  "Dutch",
  "Indonesian",
  "Vietnamese",
  "Turkish",
  "Arabic",
];

/** Added to a prompt when the user selects a tone in the AI menu. */
export const toneInstruction = (tone: string) =>
  `Use this tone: ${tone}.`;

const toneSkills = (icon?: Icon): AiSkill[] =>
  TONES.map((tone) => ({
    label: tone,
    icon,
    run: {
      instruction: `Change the tone of the text to: ${tone}. Keep the meaning, the language and the markdown formatting.`,
    },
  }));

const languageSkills = (icon?: Icon): AiSkill[] =>
  LANGUAGES.map((language) => ({
    label: language,
    icon,
    run: {
      instruction: `Translate the text to ${language}. Keep the markdown formatting.`,
    },
  }));

const FIX: AiRun = {
  instruction:
    "Correct the spelling, grammar and punctuation. Do not change anything else.",
};
const LONGER: AiRun = {
  instruction:
    "Make the text longer with more detail. Keep the tone and the language.",
};
const SHORTER: AiRun = {
  instruction:
    "Make the text shorter. Keep the important points and the language.",
};
const SIMPLER: AiRun = {
  instruction:
    "Simplify the text: use short sentences and common words. Keep the meaning, the language and the markdown formatting.",
};
const EMOJIFY: AiRun = {
  instruction:
    "Add emojis that fit the text. Do not change the words. Keep the markdown formatting.",
};
const EXPLAIN: AiRun = {
  instruction:
    "Explain the text in simple words. Use the language of the text.",
  readOnly: true,
};

/** The "Improve" dropdown of the selection toolbar. `null` is "Ask AI". */
export const IMPROVE_GROUPS: (AiSkill | null)[][] = [
  [
    { label: "Adjust tone", icon: MicVocalIcon, items: toneSkills() },
    { label: "Fix spelling & grammar", icon: SpellCheckIcon, run: FIX },
    { label: "Extend text", icon: ArrowDownWideNarrowIcon, run: LONGER },
    { label: "Reduce text", icon: ArrowUpNarrowWideIcon, run: SHORTER },
    { label: "Simplify text", icon: FeatherIcon, run: SIMPLER },
    { label: "Emojify", icon: SmilePlusIcon, run: EMOJIFY },
  ],
  [
    null,
    {
      label: "Complete sentence",
      icon: PenLineIcon,
      run: {
        instruction:
          "Complete the text: finish the last sentence, or add one sentence if it is complete. Return the full text. Keep the tone and the language.",
      },
    },
    {
      label: "Summarize",
      icon: ListCollapseIcon,
      run: { instruction: "Summarize the text. Use the language of the text." },
    },
    { label: "Explain", icon: InfoIcon, run: EXPLAIN },
    { label: "Translate", icon: LanguagesIcon, items: languageSkills() },
  ],
];

/** The suggestion list of the AI menu, for a selection. */
export const SELECTION_SKILL_GROUPS: AiSkillGroup[] = [
  {
    label: "Edit",
    skills: [
      {
        label: "Adjust Tone",
        icon: MicVocalIcon,
        items: toneSkills(MicVocalIcon),
      },
      { label: "Fix spelling & grammar", icon: SpellCheckIcon, run: FIX },
      { label: "Make longer", icon: ArrowDownWideNarrowIcon, run: LONGER },
      { label: "Make shorter", icon: ArrowUpNarrowWideIcon, run: SHORTER },
      { label: "Simplify language", icon: FeatherIcon, run: SIMPLER },
      {
        label: "Improve writing",
        icon: WandSparklesIcon,
        run: {
          instruction:
            "Improve the writing: make it clearer and easier to read. Keep the meaning, the language and the markdown formatting.",
        },
      },
      { label: "Emojify", icon: SmilePlusIcon, run: EMOJIFY },
    ],
  },
  {
    label: "Write",
    skills: [
      {
        label: "Continue writing",
        icon: PenLineIcon,
        run: {
          instruction:
            "Continue the text. Return the text and one or two new paragraphs that follow it. Keep the tone and the language.",
        },
      },
      {
        label: "Add a summary",
        icon: ListCollapseIcon,
        run: {
          instruction:
            "Add a short summary below the text. Return the text and the summary. Use the language of the text.",
        },
      },
      { label: "Explain", icon: InfoIcon, run: EXPLAIN },
      {
        label: "Languages",
        icon: LanguagesIcon,
        items: languageSkills(LanguagesIcon),
      },
    ],
  },
];

/** The suggestion list of the AI menu, at the cursor (no selection). */
export const CURSOR_SKILL_GROUPS: AiSkillGroup[] = [
  {
    label: "Write",
    skills: [
      {
        label: "Continue writing",
        icon: PenLineIcon,
        run: {
          instruction:
            "Continue writing at the cursor with one or two paragraphs. Keep the tone and the language of the document.",
        },
      },
      {
        label: "Add a summary",
        icon: ListCollapseIcon,
        run: {
          instruction:
            "Write a short summary of the document. Use the language of the document.",
        },
      },
    ],
  },
];
