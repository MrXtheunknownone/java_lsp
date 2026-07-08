package dev.javalsp.testbed;

import dev.javalsp.testbed.util.Greeter;

public class Main {
    public static void main(String[] args) {
        final Greeter greeter = new Greeter("java-lsp");
        System.out.println(greeter.greet());
        final Person person = new Person(55, "Hans");
        person.sayHello();
    }
}
