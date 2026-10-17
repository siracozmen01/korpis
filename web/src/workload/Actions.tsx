// SPDX-License-Identifier: AGPL-3.0-or-later

// A workload's actions, each interrupting as much as its plan's risk calls for (§4 of
// docs/11-surface-web.md, tier.ts): applied at once and shown after, previewed in place with one
// confirmation, or reviewed on a screen of its own with an explicit confirmation. A plan that
// needs someone else's authority (§3.6 of docs/08-identity.md) is left waiting for them, and the
// screen says so.

import { useState } from "react";
import { Button, CheckboxButton, CheckboxField } from "react-aria-components";
import { useReach } from "../clients";
import type { WorkloadStatus } from "../gen/korpis/v1/api_pb";
import { PlanApproval, PlanStatus, type Plan } from "../gen/korpis/v1/workload_pb";
import { refusal } from "../views/refusal";
import { type Action, LABEL, confirm, offered, propose, withdraw } from "./act";
import { PlanView } from "./PlanView";
import { tierOf } from "./tier";

type Step =
  | { readonly kind: "idle" }
  | { readonly kind: "busy" }
  | { readonly kind: "preview"; readonly action: Action; readonly plan: Plan }
  | { readonly kind: "review"; readonly action: Action; readonly plan: Plan }
  | { readonly kind: "waiting"; readonly plan: Plan }
  | { readonly kind: "done"; readonly action: Action; readonly plan: Plan };

/** The actions on the workload of `status`. */
export function Actions({ status }: { readonly status: WorkloadStatus }) {
  const { clients, allowed } = useReach();
  const [step, setStep] = useState<Step>({ kind: "idle" });
  const [error, setError] = useState<string>();
  const run = (work: () => Promise<Step>) => {
    setError(undefined);
    setStep({ kind: "busy" });
    work().then(setStep, (e: unknown) => {
      setError(refusal(e));
      setStep({ kind: "idle" });
    });
  };
  const confirmed = (action: Action, plan: Plan) => async (): Promise<Step> => ({
    kind: "done",
    action,
    plan: (await confirm(clients, plan)) ?? plan,
  });
  const begin = (action: Action) => {
    run(async () => {
      const plan = await propose(clients, status, action);
      if (plan === undefined) throw new Error("the control plane sent no plan");
      const waits = plan.status !== PlanStatus.PROPOSED || plan.approval === PlanApproval.AUTHORITY;
      if (waits) return { kind: "waiting", plan };
      const tier = tierOf(plan);
      if (tier === "direct") return confirmed(action, plan)();
      return { kind: tier === "confirm" ? "preview" : "review", action, plan };
    });
  };
  const cancel = (plan: Plan) => {
    run(async () => {
      await withdraw(clients, plan);
      return { kind: "idle" };
    });
  };
  const alert = error !== undefined && <p role="alert">{error}</p>;

  if (step.kind === "review") {
    return (
      <Review
        step={step}
        alert={alert}
        confirm={() => {
          run(confirmed(step.action, step.plan));
        }}
        cancel={() => {
          cancel(step.plan);
        }}
      />
    );
  }
  return (
    <section aria-label="Actions">
      {offered(status, allowed).map((a) => (
        <Button
          key={a}
          isDisabled={step.kind !== "idle" && step.kind !== "done"}
          onPress={() => {
            begin(a);
          }}
        >
          {LABEL[a]}
        </Button>
      ))}
      {step.kind === "preview" && (
        <div aria-label="Preview">
          <p>{LABEL[step.action]} will do this:</p>
          <PlanView plan={step.plan} />
          <Button
            onPress={() => {
              run(confirmed(step.action, step.plan));
            }}
          >
            Confirm
          </Button>
          <Button
            onPress={() => {
              cancel(step.plan);
            }}
          >
            Cancel
          </Button>
        </div>
      )}
      {step.kind === "waiting" && (
        <p role="status">
          Its plan waits for approval:{" "}
          {step.plan.impact?.approvalReason ?? "someone must approve it"}
        </p>
      )}
      {step.kind === "done" && (
        <details>
          <summary>{LABEL[step.action]}: done. What it did</summary>
          <PlanView plan={step.plan} />
        </details>
      )}
      {alert}
    </section>
  );
}

/** The whole plan, on its own, with an explicit confirmation. */
function Review(props: {
  readonly step: { readonly action: Action; readonly plan: Plan };
  readonly alert: React.ReactNode;
  readonly confirm: () => void;
  readonly cancel: () => void;
}) {
  const [read, setRead] = useState(false);
  return (
    <section aria-labelledby="review">
      <h2 id="review">Review: {LABEL[props.step.action]}</h2>
      <p>Every step it takes, and every object it acts on:</p>
      <PlanView plan={props.step.plan} />
      <CheckboxField isSelected={read} onChange={setRead}>
        <CheckboxButton>
          I have read every step, and what is marked irreversible cannot be undone
        </CheckboxButton>
      </CheckboxField>
      <Button isDisabled={!read} onPress={props.confirm}>
        {LABEL[props.step.action]}
      </Button>
      <Button onPress={props.cancel}>Cancel</Button>
      {props.alert}
    </section>
  );
}
