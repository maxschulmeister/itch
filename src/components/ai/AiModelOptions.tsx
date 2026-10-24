import { Fragment, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  fitThinkingLevel,
  listAiModels,
  thinkingLabel,
  type AiModel,
  type AiModelList,
  type AiProvider,
} from "../../services/inlineAi";

export type AiModelsState =
  | { status: "loading" }
  | ({ status: "ready" } & AiModelList)
  | { status: "error"; message: string };

/**
 * The models of `provider`, and a function that asks the tool again (for
 * example after an error, or after a new model download).
 */
export function useAiModels(
  provider: AiProvider | null,
): [AiModelsState, () => void] {
  const [state, setState] = useState<AiModelsState>({ status: "loading" });
  // Only the answer to the last request counts.
  const requestRef = useRef(0);

  const load = useCallback((tool: AiProvider, refresh: boolean) => {
    const request = ++requestRef.current;
    setState({ status: "loading" });
    listAiModels(tool, refresh)
      .then(
        (list) =>
          request === requestRef.current &&
          setState({ status: "ready", ...list }),
      )
      .catch(
        (err) =>
          request === requestRef.current &&
          setState({ status: "error", message: String(err) }),
      );
  }, []);

  useEffect(() => {
    if (!provider) return;
    load(provider, false);
    return () => {
      requestRef.current += 1;
    };
  }, [provider, load]);

  const retry = useCallback(() => {
    if (provider) load(provider, true);
  }, [provider, load]);

  return [state, retry];
}

/** The model with the id `value`, or undefined while the list is not ready. */
export function findAiModel(
  state: AiModelsState,
  value: string,
): AiModel | undefined {
  if (state.status !== "ready" || !value) return undefined;
  return state.models.find((model) => model.id === value);
}

/** The thinking levels of `model`. An empty `model` is the tool default. */
export function modelThinkingLevels(
  state: AiModelsState,
  model: string,
): string[] {
  const id = model || (state.status === "ready" ? state.defaultModel : null);
  return findAiModel(state, id ?? "")?.thinkingLevels ?? [];
}

/**
 * The start model and thinking level: the saved values when the tool still
 * offers them, else the tool default (an empty string).
 */
export function resolveAiDefaults(
  state: AiModelsState,
  saved: { model?: string; thinking?: string },
): { model: string; thinking: string } {
  if (state.status !== "ready") {
    return { model: saved.model ?? "", thinking: saved.thinking ?? "" };
  }
  const model = findAiModel(state, saved.model ?? "") ? saved.model! : "";
  return {
    model,
    thinking: fitThinkingLevel(
      saved.thinking ?? "",
      modelThinkingLevels(state, model),
    ),
  };
}

/** The label of the "Default" entry, with the default model when known. */
function defaultModelLabel(state: AiModelsState): string {
  const name =
    state.status === "ready"
      ? findAiModel(state, state.defaultModel ?? "")?.name
      : undefined;
  return name ? `Default (${name})` : "Default";
}

/** The name of `model` for a select button. */
export function modelLabel(state: AiModelsState, model: string): string {
  if (!model) return defaultModelLabel(state);
  return findAiModel(state, model)?.name ?? model;
}

/**
 * `<option>` elements for a model select: "Default" (the tool default),
 * then one group per model group.
 */
export function AiModelOptions({ state }: { state: AiModelsState }) {
  const groups = useMemo(() => {
    const byGroup = new Map<string, AiModel[]>();
    if (state.status === "ready") {
      for (const model of state.models) {
        const list = byGroup.get(model.group) ?? [];
        list.push(model);
        byGroup.set(model.group, list);
      }
    }
    return [...byGroup];
  }, [state]);

  const options = (models: AiModel[]) =>
    models.map((model) => (
      <option key={model.id} value={model.id}>
        {model.name}
      </option>
    ));

  return (
    <>
      <option value="">{defaultModelLabel(state)}</option>
      {groups.map(([group, models]) =>
        group ? (
          <optgroup key={group} label={group}>
            {options(models)}
          </optgroup>
        ) : (
          <Fragment key="">{options(models)}</Fragment>
        ),
      )}
    </>
  );
}

/** `<option>` elements for the thinking levels of a model. */
export function AiThinkingOptions({ levels }: { levels: string[] }) {
  return (
    <>
      <option value="">Default</option>
      {levels.map((level) => (
        <option key={level} value={level}>
          {thinkingLabel(level)}
        </option>
      ))}
    </>
  );
}
