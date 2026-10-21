import { useCallback, useEffect, useMemo, useState } from "react";
import { listPiModels, piModelKey, type PiModel } from "../../services/pi";

export type PiModelsState =
  | { status: "loading" }
  | { status: "ready"; models: PiModel[] }
  | { status: "error"; message: string };

/** The models of pi, and a function that loads them again after an error. */
export function usePiModels(): [PiModelsState, () => void] {
  const [state, setState] = useState<PiModelsState>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);

  useEffect(() => {
    let current = true;
    listPiModels()
      .then((models) => current && setState({ status: "ready", models }))
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

/** The model of `value`, or undefined for the pi default or an unknown model. */
export function findPiModel(
  state: PiModelsState,
  value: string,
): PiModel | undefined {
  if (state.status !== "ready" || !value) return undefined;
  return state.models.find((model) => piModelKey(model) === value);
}

/**
 * `<option>` elements for a model select: the pi default, then one group per
 * provider. A saved model that pi does not list stays selectable.
 */
export function PiModelOptions({
  state,
  value,
}: {
  state: PiModelsState;
  value: string;
}) {
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

  const unknown = value !== "" && !findPiModel(state, value);

  return (
    <>
      <option value="">Default (pi)</option>
      {unknown && <option value={value}>{value}</option>}
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
