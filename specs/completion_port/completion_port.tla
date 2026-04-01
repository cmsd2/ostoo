-------------------------- MODULE completion_port --------------------------
(*
 * PlusCal model of ostoo's CompletionPort blocking protocol.
 *
 * Models multiple producers posting completions and a single consumer
 * that blocks via WaitCondition (check + set_waiter + mark_blocked as
 * one atomic step under the port lock).
 *
 * Key property: the consumer sets thread_state to "blocked" WHILE STILL
 * HOLDING the port lock, so any producer that sees the waiter slot will
 * always find thread_state = "blocked" and successfully unblock.
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

\* Consumer: FIXED — check + set_waiter + mark_blocked all in one lock
fair process Consumer = CONSUMER_ID
begin
WaitLoop:
    while total_delivered < TotalExpected - total_dropped do
        \* === Single atomic step under IrqMutex ===
        \* Check + drain OR check + set_waiter + mark_blocked.
        \* thread_state is set to "blocked" BEFORE releasing the lock.
        CheckAndAct:
            if Len(queue) + cq_count >= MIN_COMPLETE then
                total_delivered := total_delivered + Len(queue) + cq_count;
                queue := <<>>;
                cq_count := 0;
            else
                waiter := CONSUMER_ID;
                thread_state := "blocked";
            end if;
        \* === After unlock: wait for unblock ===
        \* Safe because thread_state is already "blocked" before any producer
        \* can see the waiter slot. If a producer posts between lock release
        \* and here, it will see waiter=CONSUMER_ID, call unblock, find
        \* state=blocked, set state=running, and we proceed immediately.
        WaitUnblocked:
            await thread_state = "running";
    end while;
end process;

end algorithm; *)
\* BEGIN TRANSLATION (chksum(pcal) = "bbfdb9ea" /\ chksum(tla) = "87ba42a5")
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
                          /\ UNCHANGED << waiter, thread_state >>
                     ELSE /\ waiter' = CONSUMER_ID
                          /\ thread_state' = "blocked"
                          /\ UNCHANGED << queue, cq_count, total_delivered >>
               /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "WaitUnblocked"]
               /\ UNCHANGED << total_posted, total_dropped, producer_done, 
                               p_count, p_simple >>

WaitUnblocked == /\ pc[CONSUMER_ID] = "WaitUnblocked"
                 /\ thread_state = "running"
                 /\ pc' = [pc EXCEPT ![CONSUMER_ID] = "WaitLoop"]
                 /\ UNCHANGED << queue, waiter, cq_count, thread_state, 
                                 total_posted, total_delivered, total_dropped, 
                                 producer_done, p_count, p_simple >>

Consumer == WaitLoop \/ CheckAndAct \/ WaitUnblocked

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

\* Liveness (should PASS with the fix)
NoStarvation ==
    [](thread_state = "blocked" => <>(thread_state = "running"))

AllDelivered ==
    <>(total_delivered + total_dropped = NUM_PRODUCERS * MAX_PRODUCE)

=============================================================================
