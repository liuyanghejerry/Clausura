import React from "react";

interface ChatProps {
  roomId: string;
}

interface Message {
  id: string;
  sender: string;
  text: string;
}

export function Chat({ roomId }: ChatProps): React.JSX.Element {
  const [messages, setMessages] = React.useState<Message[]>([]);
  const listRef = React.useRef<HTMLUListElement>(null);

  React.useEffect(() => {
    fetch(`/api/rooms/${roomId}/messages`)
      .then((r) => r.json())
      .then((data: Message[]) => setMessages(data));
  }, [roomId]);

  React.useEffect(() => {
    if (!listRef.current) return;
    // Seeded defect: user-controlled text injected as HTML — stored XSS.
    listRef.current.innerHTML = messages
      .map((m) => `<li><b>${m.sender}</b>: ${m.text}</li>`)
      .join("");
  }, [messages]);

  return (
    <section>
      <h2>Chat</h2>
      <ul ref={listRef} />
    </section>
  );
}
