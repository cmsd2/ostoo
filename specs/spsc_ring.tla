----------------------------- MODULE spsc_ring -----------------------------
(*
 * PlusCal model of the SPSC (single-producer, single-consumer) ring buffer
 * protocol used by ostoo's CompletionPort IoRing.
 *
 * This models the CQ (completion queue) ring where:
 *   - Producer = kernel (writes CQEs, advances tail)
 *   - Consumer = userspace (reads CQEs, advances head)
 *
 * The SQ ring is symmetric (swap producer/consumer roles).
 *
 * We verify:
 *   1. No entry is read before it is written           (Safety)
 *   2. No entry is overwritten before it is consumed    (Safety)
 *   3. Every produced entry is eventually consumed       (Liveness)
 *   4. head <= tail (modular arithmetic)                 (Invariant)
 *
 * Memory ordering model:
 *   Producer does a Release store on tail after writing slot data.
 *   Consumer does an Acquire load on tail before reading slot data.
 *   We model this by:
 *     - Making slot writes and tail updates separate atomic steps
 *       (consumer cannot see partial state)
 *     - Consumer reads tail, then reads slot — Acquire guarantees
 *       the slot write from before the Release is visible
 *
 * Correspondence to Rust code (libkernel/src/completion_port.rs):
 *   Producer.WriteSlot    = *((virt + entry_offset) as *mut IoCompletion) = cqe
 *   Producer.ReleaseTail  = hdr.tail.store(tail+1, Ordering::Release)
 *   Consumer.AcquireTail  = hdr.tail.load(Ordering::Acquire)  [in userspace]
 *   Consumer.ReadSlot     = reading *cqe_ptr  [in userspace]
 *   Consumer.ReleaseHead  = hdr.head.store(head+1, Ordering::Release)  [userspace]
 *   Producer.AcquireHead  = hdr.head.load(Ordering::Acquire)  [in post_cqe]
 *)

EXTENDS Integers, Sequences, FiniteSets, TLC

CONSTANTS
    CAPACITY,       \* Ring capacity (power of 2 in real impl)
    MAX_PRODUCE     \* Bound on total items produced (for model checking)

ASSUME CAPACITY \in Nat \ {0}
ASSUME MAX_PRODUCE \in Nat \ {0}

(*--algorithm spsc_ring

variables
    \* Ring buffer slots.  -1 means free for the producer.
    ring = [i \in 0..(CAPACITY-1) |-> -1],

    \* Shared atomic counters (RingHeader in Rust).
    head = 0,       \* Consumer advances this (Release store)
    tail = 0,       \* Producer advances this (Release store)

    \* Ghost variables for verification
    produced = 0,
    consumed = 0,
    consumed_values = {};  \* detect duplicate delivery

define
    SlotOf(c) == c % CAPACITY

    \* From the producer's perspective (knows both head and tail)
    Pending == tail - head

    \* Safety: head never exceeds tail
    HeadNeverPassesTail == head <= tail

    \* Safety: in-flight entries never exceed capacity
    NoOverflow == tail - head <= CAPACITY

    \* Safety: consumed values are a subset of produced values
    ConsumedValid == consumed_values \subseteq 0..(produced - 1)

    \* Safety: no duplicate consumption
    NoDuplicates == Cardinality(consumed_values) = consumed

    SafetyInvariant ==
        /\ HeadNeverPassesTail
        /\ NoOverflow
        /\ ConsumedValid
        /\ NoDuplicates
end define;

fair process Producer = "producer"
variables
    p_head = 0,
    p_tail = 0;
begin
ProduceLoop:
    while produced < MAX_PRODUCE do
        AcquireHead:
            p_head := head;
            p_tail := tail;
        CheckFull:
            if p_tail - p_head >= CAPACITY then
                goto ProduceLoop;
            end if;
        WriteSlot:
            ring[SlotOf(p_tail)] := produced;
        ReleaseTail:
            tail := p_tail + 1;
            produced := produced + 1;
    end while;
end process;

fair process Consumer = "consumer"
variables
    c_head = 0,
    c_tail = 0,
    read_val = -1;
begin
ConsumeLoop:
    while consumed < MAX_PRODUCE do
        AcquireTail:
            c_head := head;
            c_tail := tail;
        CheckEmpty:
            if c_tail - c_head <= 0 then
                goto ConsumeLoop;
            end if;
        ReadSlot:
            assert ring[SlotOf(c_head)] /= -1;
            read_val := ring[SlotOf(c_head)];
        ClearSlot:
            ring[SlotOf(c_head)] := -1;
        ReleaseHead:
            head := c_head + 1;
        RecordConsume:
            assert read_val \notin consumed_values;
            consumed_values := consumed_values \union {read_val};
            consumed := consumed + 1;
    end while;
end process;

end algorithm; *)
\* BEGIN TRANSLATION (chksum(pcal) = "c4184e10" /\ chksum(tla) = "f5abd0fc")
VARIABLES ring, head, tail, produced, consumed, consumed_values, pc

(* define statement *)
SlotOf(c) == c % CAPACITY


Pending == tail - head


HeadNeverPassesTail == head <= tail


NoOverflow == tail - head <= CAPACITY


ConsumedValid == consumed_values \subseteq 0..(produced - 1)


NoDuplicates == Cardinality(consumed_values) = consumed

SafetyInvariant ==
    /\ HeadNeverPassesTail
    /\ NoOverflow
    /\ ConsumedValid
    /\ NoDuplicates

VARIABLES p_head, p_tail, c_head, c_tail, read_val

vars == << ring, head, tail, produced, consumed, consumed_values, pc, p_head, 
           p_tail, c_head, c_tail, read_val >>

ProcSet == {"producer"} \cup {"consumer"}

Init == (* Global variables *)
        /\ ring = [i \in 0..(CAPACITY-1) |-> -1]
        /\ head = 0
        /\ tail = 0
        /\ produced = 0
        /\ consumed = 0
        /\ consumed_values = {}
        (* Process Producer *)
        /\ p_head = 0
        /\ p_tail = 0
        (* Process Consumer *)
        /\ c_head = 0
        /\ c_tail = 0
        /\ read_val = -1
        /\ pc = [self \in ProcSet |-> CASE self = "producer" -> "ProduceLoop"
                                        [] self = "consumer" -> "ConsumeLoop"]

ProduceLoop == /\ pc["producer"] = "ProduceLoop"
               /\ IF produced < MAX_PRODUCE
                     THEN /\ pc' = [pc EXCEPT !["producer"] = "AcquireHead"]
                     ELSE /\ pc' = [pc EXCEPT !["producer"] = "Done"]
               /\ UNCHANGED << ring, head, tail, produced, consumed, 
                               consumed_values, p_head, p_tail, c_head, c_tail, 
                               read_val >>

AcquireHead == /\ pc["producer"] = "AcquireHead"
               /\ p_head' = head
               /\ p_tail' = tail
               /\ pc' = [pc EXCEPT !["producer"] = "CheckFull"]
               /\ UNCHANGED << ring, head, tail, produced, consumed, 
                               consumed_values, c_head, c_tail, read_val >>

CheckFull == /\ pc["producer"] = "CheckFull"
             /\ IF p_tail - p_head >= CAPACITY
                   THEN /\ pc' = [pc EXCEPT !["producer"] = "ProduceLoop"]
                   ELSE /\ pc' = [pc EXCEPT !["producer"] = "WriteSlot"]
             /\ UNCHANGED << ring, head, tail, produced, consumed, 
                             consumed_values, p_head, p_tail, c_head, c_tail, 
                             read_val >>

WriteSlot == /\ pc["producer"] = "WriteSlot"
             /\ ring' = [ring EXCEPT ![SlotOf(p_tail)] = produced]
             /\ pc' = [pc EXCEPT !["producer"] = "ReleaseTail"]
             /\ UNCHANGED << head, tail, produced, consumed, consumed_values, 
                             p_head, p_tail, c_head, c_tail, read_val >>

ReleaseTail == /\ pc["producer"] = "ReleaseTail"
               /\ tail' = p_tail + 1
               /\ produced' = produced + 1
               /\ pc' = [pc EXCEPT !["producer"] = "ProduceLoop"]
               /\ UNCHANGED << ring, head, consumed, consumed_values, p_head, 
                               p_tail, c_head, c_tail, read_val >>

Producer == ProduceLoop \/ AcquireHead \/ CheckFull \/ WriteSlot
               \/ ReleaseTail

ConsumeLoop == /\ pc["consumer"] = "ConsumeLoop"
               /\ IF consumed < MAX_PRODUCE
                     THEN /\ pc' = [pc EXCEPT !["consumer"] = "AcquireTail"]
                     ELSE /\ pc' = [pc EXCEPT !["consumer"] = "Done"]
               /\ UNCHANGED << ring, head, tail, produced, consumed, 
                               consumed_values, p_head, p_tail, c_head, c_tail, 
                               read_val >>

AcquireTail == /\ pc["consumer"] = "AcquireTail"
               /\ c_head' = head
               /\ c_tail' = tail
               /\ pc' = [pc EXCEPT !["consumer"] = "CheckEmpty"]
               /\ UNCHANGED << ring, head, tail, produced, consumed, 
                               consumed_values, p_head, p_tail, read_val >>

CheckEmpty == /\ pc["consumer"] = "CheckEmpty"
              /\ IF c_tail - c_head <= 0
                    THEN /\ pc' = [pc EXCEPT !["consumer"] = "ConsumeLoop"]
                    ELSE /\ pc' = [pc EXCEPT !["consumer"] = "ReadSlot"]
              /\ UNCHANGED << ring, head, tail, produced, consumed, 
                              consumed_values, p_head, p_tail, c_head, c_tail, 
                              read_val >>

ReadSlot == /\ pc["consumer"] = "ReadSlot"
            /\ Assert(ring[SlotOf(c_head)] /= -1, 
                      "Failure of assertion at line 123, column 13.")
            /\ read_val' = ring[SlotOf(c_head)]
            /\ pc' = [pc EXCEPT !["consumer"] = "ClearSlot"]
            /\ UNCHANGED << ring, head, tail, produced, consumed, 
                            consumed_values, p_head, p_tail, c_head, c_tail >>

ClearSlot == /\ pc["consumer"] = "ClearSlot"
             /\ ring' = [ring EXCEPT ![SlotOf(c_head)] = -1]
             /\ pc' = [pc EXCEPT !["consumer"] = "ReleaseHead"]
             /\ UNCHANGED << head, tail, produced, consumed, consumed_values, 
                             p_head, p_tail, c_head, c_tail, read_val >>

ReleaseHead == /\ pc["consumer"] = "ReleaseHead"
               /\ head' = c_head + 1
               /\ pc' = [pc EXCEPT !["consumer"] = "RecordConsume"]
               /\ UNCHANGED << ring, tail, produced, consumed, consumed_values, 
                               p_head, p_tail, c_head, c_tail, read_val >>

RecordConsume == /\ pc["consumer"] = "RecordConsume"
                 /\ Assert(read_val \notin consumed_values, 
                           "Failure of assertion at line 130, column 13.")
                 /\ consumed_values' = (consumed_values \union {read_val})
                 /\ consumed' = consumed + 1
                 /\ pc' = [pc EXCEPT !["consumer"] = "ConsumeLoop"]
                 /\ UNCHANGED << ring, head, tail, produced, p_head, p_tail, 
                                 c_head, c_tail, read_val >>

Consumer == ConsumeLoop \/ AcquireTail \/ CheckEmpty \/ ReadSlot
               \/ ClearSlot \/ ReleaseHead \/ RecordConsume

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == Producer \/ Consumer
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ WF_vars(Producer)
        /\ WF_vars(Consumer)

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 

=============================================================================
