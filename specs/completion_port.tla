-------------------------- MODULE completion_port --------------------------
(*
 * PlusCal model of ostoo's CompletionPort kernel object.
 *
 * FINDING: This spec exposes a lost-wakeup race in the real code.
 *
 * The race (confirmed by reading scheduler.rs lines 640-676):
 *   1. Consumer: lock, check, set_waiter(self), unlock
 *   2. Producer: lock, post(), sees waiter, calls unblock(consumer)
 *      -> BUT consumer thread state is still Running, not Blocked
 *      -> unblock() checks "if state == Blocked" -- it's not -- NO-OP
 *   3. Consumer: block_current_thread() sets state = Blocked, spins forever
 *      -> waiter slot was cleared in step 2 -- no future post will wake us
 *      -> DEADLOCK
 *
 * The fix: see completion_port_fixed.tla
 *)

EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS
    NUM_PRODUCERS,
    MAX_QUEUED,
    MIN_COMPLETE,
    MAX_PRODUCE

ASSUME NUM_PRODUCERS \in Nat \ {0}
ASSUME MAX_QUEUED \in Nat \ {0}
ASSUME MIN_COMPLETE \in Nat \ {0}
ASSUME MAX_PRODUCE \in Nat \ {0}

Producers == 1..NUM_PRODUCERS
CONSUMER_ID == 0

(*--algorithm completion_port

variables
    queue = <<>>,
    waiter = -1,
    cq_count = 0,
    thread_state = "running",
    total_posted = 0,
    total_delivered = 0,
    total_dropped = 0,
    producer_done = [p \in Producers |-> FALSE];

define
    AllDone == \A p \in Producers : producer_done[p]
    TotalExpected == NUM_PRODUCERS * MAX_PRODUCE

    SingleWaiter == waiter \in {-1, CONSUMER_ID}
    QueueBounded == Len(queue) <= MAX_QUEUED
    AccountingSound ==
        total_posted = total_delivered + Len(queue) + cq_count + total_dropped

    SafetyInvariant ==
        /\ SingleWaiter
        /\ QueueBounded
        /\ AccountingSound
end define;

fair process Producer \in Producers
variables
    p_count = 0,
    p_simple = TRUE;
begin
ProdLoop:
    while p_count < MAX_PRODUCE do
        ChooseType:
            with s \in {TRUE, FALSE} do
                p_simple := s;
            end with;
        \* Atomic: port.lock().post(completion)
        Post:
            if p_simple then
                cq_count := cq_count + 1;
            else
                if Len(queue) < MAX_QUEUED then
                    queue := Append(queue, total_posted);
                else
                    total_dropped := total_dropped + 1;
                end if;
            end if;
            total_posted := total_posted + 1;
            if waiter = CONSUMER_ID then
                waiter := -1;
                if thread_state = "blocked" then
                    thread_state := "running";
                end if;
            end if;
        Advance:
            p_count := p_count + 1;
    end while;
    ProdDone:
        producer_done[self] := TRUE;
end process;

fair process Consumer = CONSUMER_ID
begin
WaitLoop:
    while total_delivered < TotalExpected - total_dropped do
        \* === Atomic: lock port, check, either drain or set_waiter, unlock ===
        \* In the real code, check + drain/set_waiter are ONE lock acquisition.
        CheckAndAct:
            if Len(queue) + cq_count >= MIN_COMPLETE then
                total_delivered := total_delivered + Len(queue) + cq_count;
                queue := <<>>;
                cq_count := 0;
            else
                waiter := CONSUMER_ID;
                \* Lock released here. block_current_thread() is AFTER unlock.
            end if;
        \* === After unlock: block_current_thread() ===
        \* Only reached if we set waiter (else branch above goes to WaitLoop).
        \* A producer may have run Post between CheckAndAct and here.
        Block:
            if waiter = -1 then
                \* Waiter was cleared by a producer — we were woken in the gap.
                \* But unblock was a no-op because we weren't blocked yet.
                \* We must still block because block_current_thread() is unconditional.
                thread_state := "blocked";
                await thread_state = "running";
            elsif waiter = CONSUMER_ID then
                \* Normal case: waiter still set, block and wait for wakeup.
                thread_state := "blocked";
                await thread_state = "running";
            end if;
    end while;
end process;

end algorithm; *)
\* BEGIN TRANSLATION (chksum(pcal) = "71abada3" /\ chksum(tla) = "5843f46d")
VARIABLES queue, waiter, cq_count, thread_state, total_posted, 
          total_delivered, total_dropped, producer_done, pc

(* define statement *)
AllDone == \A p \in Producers : producer_done[p]
TotalExpected == NUM_PRODUCERS * MAX_PRODUCE

SingleWaiter == waiter \in {-1, CONSUMER_ID}
QueueBounded == Len(queue) <= MAX_QUEUED
AccountingSound ==
    total_posted = total_delivered + Len(queue) + cq_count + total_dropped

SafetyInvariant ==
    /\ SingleWaiter
    /\ QueueBounded
    /\ AccountingSound

VARIABLES p_count, p_simple

vars == << queue, waiter, cq_count, thread_state, total_posted, 
           total_delivered, total_dropped, producer_done, pc, p_count, 
           p_simple >>

ProcSet == (Producers) \cup {CONSUMER_ID}

Init == (* Global variables *)
        /\ queue = <<>>
        /\ waiter = -1
        /\ cq_count = 0
        /\ thread_state = "running"
        /\ total_posted = 0
        /\ total_delivered = 0
        /\ total_dropped = 0
        /\ producer_done = [p \in Producers |-> FALSE]
        (* Process Producer *)
        /\ p_count = [self \in Producers |-> 0]
        /\ p_simple = [self \in Producers |-> TRUE]
        /\ pc = [self \in ProcSet |-> CASE self \in Producers -> "ProdLoop"
                                        [] self = CONSUMER_ID -> "WaitLoop"]

ProdLoop(self) == /\ pc[self] = "ProdLoop"
                  /\ IF p_count[self] < MAX_PRODUCE
                        THEN /\ pc' = [pc EXCEPT ![self] = "ChooseType"]
                        ELSE /\ pc' = [pc EXCEPT ![self] = "ProdDone"]
                  /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                                  total_posted, total_delivered, total_dropped, 
                                  producer_done, p_count, p_simple >>

ChooseType(self) == /\ pc[self] = "ChooseType"
                    /\ \E s \in {TRUE, FALSE}:
                         p_simple' = [p_simple EXCEPT ![self] = s]
                    /\ pc' = [pc EXCEPT ![self] = "Post"]
                    /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                                    total_posted, total_delivered, 
                                    total_dropped, producer_done, p_count >>

Post(self) == /\ pc[self] = "Post"
              /\ IF p_simple[self]
                    THEN /\ cq_count' = cq_count + 1
                         /\ UNCHANGED << queue, total_dropped >>
                    ELSE /\ IF Len(queue) < MAX_QUEUED
                               THEN /\ queue' = Append(queue, total_posted)
                                    /\ UNCHANGED total_dropped
                               ELSE /\ total_dropped' = total_dropped + 1
                                    /\ queue' = queue
                         /\ UNCHANGED cq_count
              /\ total_posted' = total_posted + 1
              /\ IF waiter = CONSUMER_ID
                    THEN /\ waiter' = -1
                         /\ IF thread_state = "blocked"
                               THEN /\ thread_state' = "running"
                               ELSE /\ TRUE
                                    /\ UNCHANGED thread_state
                    ELSE /\ TRUE
                         /\ UNCHANGED << waiter, thread_state >>
              /\ pc' = [pc EXCEPT ![self] = "Advance"]
              /\ UNCHANGED << total_delivered, producer_done, p_count, 
                              p_simple >>

Advance(self) == /\ pc[self] = "Advance"
                 /\ p_count' = [p_count EXCEPT ![self] = p_count[self] + 1]
                 /\ pc' = [pc EXCEPT ![self] = "ProdLoop"]
                 /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                                 total_posted, total_delivered, total_dropped, 
                                 producer_done, p_simple >>

ProdDone(self) == /\ pc[self] = "ProdDone"
                  /\ producer_done' = [producer_done EXCEPT ![self] = TRUE]
                  /\ pc' = [pc EXCEPT ![self] = "Done"]
                  /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                                  total_posted, total_delivered, total_dropped, 
                                  p_count, p_simple >>

Producer(self) == ProdLoop(self) \/ ChooseType(self) \/ Post(self)
                     \/ Advance(self) \/ ProdDone(self)

WaitLoop == /\ pc[CONSUMER_ID] = "WaitLoop"
            /\ IF total_delivered < TotalExpected - total_dropped
                  THEN /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "CheckAndAct"]
                  ELSE /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "Done"]
            /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                            total_posted, total_delivered, total_dropped, 
                            producer_done, p_count, p_simple >>

CheckAndAct == /\ pc[CONSUMER_ID] = "CheckAndAct"
               /\ IF Len(queue) + cq_count >= MIN_COMPLETE
                     THEN /\ total_delivered' = total_delivered + Len(queue) + cq_count
                          /\ queue' = <<>>
                          /\ cq_count' = 0
                          /\ UNCHANGED waiter
                     ELSE /\ waiter' = CONSUMER_ID
                          /\ UNCHANGED << queue, cq_count, total_delivered >>
               /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "Block"]
               /\ UNCHANGED << thread_state, total_posted, total_dropped, 
                               producer_done, p_count, p_simple >>

Block == /\ pc[CONSUMER_ID] = "Block"
         /\ IF waiter = -1
               THEN /\ thread_state' = "blocked"
                    /\ thread_state' = "running"
               ELSE /\ IF waiter = CONSUMER_ID
                          THEN /\ thread_state' = "blocked"
                               /\ thread_state' = "running"
                          ELSE /\ TRUE
                               /\ UNCHANGED thread_state
         /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "WaitLoop"]
         /\ UNCHANGED << queue, waiter, cq_count, total_posted, 
                         total_delivered, total_dropped, producer_done, 
                         p_count, p_simple >>

Consumer == WaitLoop \/ CheckAndAct \/ Block

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == Consumer
           \/ (\E self \in Producers: Producer(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ \A self \in Producers : WF_vars(Producer(self))
        /\ WF_vars(Consumer)

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 

\* Liveness (TLC should find VIOLATED due to lost-wakeup bug)
NoStarvation ==
    [](thread_state = "blocked" => <>(thread_state = "running"))

AllDelivered ==
    <>(total_delivered + total_dropped = NUM_PRODUCERS * MAX_PRODUCE)

=============================================================================
