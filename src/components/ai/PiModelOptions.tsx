import { useCallback, useEffect, useMemo, useState } from "react";
import {
  fitThinkingLevel,
  listPiModels,
  piModelKey,
  PI_THINKING_LEVELS,
  type PiModel,
  type PiModelList,
} from "../../services/pi";
import type { Settings } from "../../types/note";

export type PiModelsState =
  | { status: "loading" }
  | ({ status: "ready" } & PiModelList)
  | { status: "error"; message: string };

/** The models of pi, and a function that loads them again after an error. */
export function usePiModels(): [PiModelsState, () => void] {
  const [state, setState] = useState<PiModelsState>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let current = true;
    listPiModels()
      .then((list) => current && setState({ status: "ready", ...list }))
      .catch(
        (err) => current && setState({ status: "error", message: String(err) }),
      );
    return () => {
      current = false;
    };
  }, [attempt]);

  const retry = useCallback(() => {
    setState({ status: "loading" });
    setAttempt((n) => n + 1);
  }, []);

  return [state, retry];
}

/** The model of `value`, or undefined while the list is not ready. */
export function findPiModel(
  state: PiModelsState,
  value: string,
): PiModel | undefined {
  if (state.status !== "ready") return undefined;
  return state.models.find((model) => piModelKey(model) === value);
}

/**
 * The default model and thinking level of Itch: the settings, else the
 * start values of pi. The level fits the model.
 */
export function resolvePiDefaults(
  list: PiModelList,
  settings: Pick<Settings, "piModel" | "piThinking">,
): { model: string; thinking: string } {
  const byKey = (key?: string | null) =>
    list.models.find((model) => piModelKey(model) === key);
  const model =
    byKey(settings.piModel) ?? byKey(list.defaultModel) ?? list.models[0];
  const level = settings.piThinking || list.defaultThinking || "medium";
  return {
    model: model ? piModelKey(model) : "",
    thinking: model ? fitThinkingLevel(level, model.thinkingLevels) : level,
  };
}

/**
 * `<option>` elements for a model select: one group per provider. While the
 * list loads or after an error, one disabled placeholder option.
 */
export function PiModelOptions({ state }: { state: PiModelsState }) {
  const groups = useMemo(() => {
    const byProvider = new Map<string, PiModel[]>();
    if (state.status === "ready") {
      for (const model of state.models) {
        const list = byProvider.get(model.provider) ?? [];
        list.push(model);
        byProvider.set(model.provider, list);
      }
    }
    return [...byProvider];
  }, [state]);

  if (groups.length === 0) {
    const label =
      state.status === "loading"
        ? "Loading…"
        : state.status === "error"
          ? "Unavailable"
          : "No models";
    return (
      <option value="" disabled>
        {label}
      </option>
    );
  }

  return (
    <>
      {groups.map(([provider, models]) => (
        <optgroup key={provider} label={provider}>
          {models.map((model) => (
            <option key={piModelKey(model)} value={piModelKey(model)}>
              {model.name}
            </option>
          ))}
        </optgroup>
      ))}
    </>
  );
}

/** `<option>` elements for the thinking levels that a model supports. */
export function PiThinkingOptions({ levels }: { levels: string[] }) {
  return (
    <>
      {PI_THINKING_LEVELS.filter((level) => levels.includes(level.value)).map(
        (level) => (
          <option key={level.value} value={level.value}>
            {level.label}
          </option>
        ),
      )}
    </>
  );
}
