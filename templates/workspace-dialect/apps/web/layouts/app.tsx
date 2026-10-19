// The layout `/notes` and `/wave` sit in. Its links reach the other client
// page without a document load, and it shows a progress bar while the next
// page is fetched. `/` is a server page, so its link loads a document.
import { Node, Outlet, navigation } from "forge";

export default function AppLayout(): Node {
  return (
    <div>
      <nav>
        <a href="/">Home</a>
        <a href="/notes">Notes</a>
        <a href="/wave">Wave</a>
      </nav>
      {navigation.pending ? <progress aria-label="Loading" /> : null}
      <main>
        <Outlet />
      </main>
    </div>
  );
}
